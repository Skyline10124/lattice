// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// 回放来源只记录协议与 provider ID：DTO、会话记录与节点记录的序列化中
/// 都不得出现 endpoint / base_url 字段；格式版本为 7。
#[test]
fn replay_origin_records_no_endpoint() {
    fn assert_no_endpoint(label: &str, text: &str) {
        for field in ["base_url", "endpoint"] {
            assert!(
                !text.contains(field),
                "{label} 不应包含 {field} 字段: {text}"
            );
        }
    }
    let dto = serde_json::to_value(origin()).unwrap();
    assert_eq!(
        dto,
        json!({"protocol":"chat-completions","provider_id":"openai"}),
        "回放来源 DTO 只含协议与 provider ID"
    );
    assert_no_endpoint("回放来源 DTO", &dto.to_string());

    let (_d, _l, db) = store();
    let (_lock, _calls) = running(&db, sid(1), tid(1), 1, 1);
    let r = records(&db);
    let session = encode(&r.sessions[&sid(1)]).unwrap();
    assert_eq!(&session[..4], &10u32.to_be_bytes(), "会话记录格式版本为 10");
    assert_no_endpoint("会话记录", &String::from_utf8_lossy(&session[4..]));
    let mut origins = 0;
    for ((_, id), node) in &r.nodes {
        let bytes = encode(node).unwrap();
        assert_eq!(
            &bytes[..4],
            &10u32.to_be_bytes(),
            "节点 {id:?} 格式版本为 10"
        );
        let text = String::from_utf8_lossy(&bytes[4..]).into_owned();
        origins += text.matches("\"origin\"").count();
        assert_no_endpoint(&format!("节点 {id:?}"), &text);
    }
    assert!(origins > 0, "至少一条节点记录写入了回放来源");
    let detail = serde_json::to_string(&detail(&db)).unwrap();
    assert_no_endpoint("turn 详情", &detail);
}

#[test]
fn attempt_sources_survive_retry_and_reopen() {
    let (_dir, layout, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    let mut input = accept(sid(1), tid(1));
    input.config.provider = provider("edge-1");
    accept_prepared(&db, &lock, input).unwrap();
    let first = AttemptOrigin {
        protocol: AttemptProtocol::ChatCompletions,
        provider_id: provider("edge-1"),
    };
    let second = AttemptOrigin {
        protocol: AttemptProtocol::Messages,
        provider_id: provider("edge-1"),
    };
    db.start_request(&lock, key(tid(1), 0, 0), first.clone(), now())
        .unwrap();
    let mut stop = stopped(
        key(tid(1), 0, 0),
        StopCause::ProviderError { retryable: true },
    );
    stop.origin = first;
    let usage = TokenUsage {
        input: Some(1),
        cache_read: Some(3),
        cache_write: Some(4),
        output: Some(2),
        reasoning: Some(9),
    };
    stop.usage = Some(usage);
    stop.cost = Some(CostEstimate {
        usd: 0.125,
        exact: false,
    });
    db.stop_attempt(&lock, stop, now()).unwrap();
    db.start_request(&lock, key(tid(1), 0, 1), second.clone(), now())
        .unwrap();
    let turn = detail(&db);
    assert_eq!(turn.steps[0].attempts[0].usage, Some(usage));
    assert_eq!(
        turn.steps[0].attempts[0].cost,
        Some(CostEstimate {
            usd: 0.125,
            exact: false
        })
    );
    assert_eq!(
        turn.steps[0].attempts[0].usage_kind,
        UsageCompleteness::Partial
    );
    assert_eq!(
        turn.steps[0].attempts[1].usage_kind,
        UsageCompleteness::Partial
    );
    assert_eq!(turn.steps[0].attempts[1].usage, None);
    assert_eq!(turn.steps[0].attempts[1].cost, None);
    let expected = [
        json!({"protocol":"chat-completions","provider_id":"edge-1"}),
        json!({"protocol":"anthropic-messages","provider_id":"edge-1"}),
    ];
    let stored = serde_json::to_value(node_at(&db, 2)).unwrap();
    for (i, expected) in expected.iter().enumerate() {
        assert_eq!(
            serde_json::to_value(&turn.steps[0].attempts[i]).unwrap()["origin"],
            *expected
        );
        assert_eq!(stored["attempts"][i]["origin"], *expected);
    }
    drop(lock);
    drop(db);
    let db = SessionStore::open(&layout).unwrap();
    assert_eq!(detail(&db), turn);
    let lock = db.try_lock_session(sid(1)).unwrap();
    let mut complete = reply(key(tid(1), 0, 1), 1);
    complete.usage = Some(usage);
    complete.usage_kind = UsageCompleteness::Complete;
    complete.cost = Some(CostEstimate {
        usd: 0.25,
        exact: true,
    });
    complete.origin = second;
    complete.message.provider_origin = Some(complete.origin.clone());
    let calls = db
        .register_calls(&lock, complete, &[iid(1)], now())
        .unwrap();
    finish(&db, &lock, &calls.bindings[0], ok_reply());
    db.close_step(
        &lock,
        CloseStep::Continue {
            key: step(tid(1), 0),
        },
        now(),
    )
    .unwrap();
    let third = AttemptOrigin {
        protocol: AttemptProtocol::GenerateContent,
        provider_id: provider("edge-1"),
    };
    db.start_request(&lock, key(tid(1), 1, 0), third.clone(), now())
        .unwrap();
    let mut complete = reply(key(tid(1), 1, 0), 0);
    complete.usage = Some(TokenUsage::default());
    complete.cost = Some(CostEstimate {
        usd: 0.5,
        exact: true,
    });
    complete.origin = third.clone();
    complete.message.provider_origin = Some(complete.origin.clone());
    db.close_step(&lock, CloseStep::Finish(complete), now())
        .unwrap();
    let final_detail = detail(&db);
    assert_eq!(
        final_detail.steps[0].attempts[1].cost,
        Some(CostEstimate {
            usd: 0.25,
            exact: true
        })
    );
    assert_eq!(
        final_detail.steps[0].attempts[1].usage_kind,
        UsageCompleteness::Complete
    );
    assert_eq!(final_detail.usage.partial_attempts, 2);
    assert_eq!(final_detail.steps[1].attempts[0].usage, None);
    assert_eq!(final_detail.steps[1].attempts[0].cost, None);
    assert_eq!(
        final_detail.usage.cost.known,
        Some(CostEstimate {
            usd: 0.375,
            exact: false
        })
    );
    assert_eq!(final_detail.usage.cost.unknown_attempts, 1);
    let turn = detail(&db);
    assert_eq!(turn.summary.outcome, PublicTurnOutcome::Completed);
    assert_eq!(turn.steps[1].attempts[0].origin, third);
    let read = db.begin_read().unwrap();
    let ConversationEntry::Step { step, .. } = read
        .entry(NodeRef {
            session_id: sid(1),
            node: NodeId(2),
        })
        .unwrap()
    else {
        panic!("step entry")
    };
    assert_eq!(step, turn.steps[0]);
    drop(read);
    drop(lock);
    drop(db);
    assert_eq!(detail(&SessionStore::open(&layout).unwrap()), turn);
}

#[test]
fn attempts_without_reply_allow_sent_and_stopped_on_reopen() {
    for state in ["reserved", "sent", "stopped"] {
        let (_dir, layout, db) = store();
        let lock = db.try_lock_session(sid(1)).unwrap();
        accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
        if state != "reserved" {
            db.start_request(&lock, key(tid(1), 0, 0), origin(), now())
                .unwrap();
        }
        if state == "stopped" {
            let mut stop = stopped(
                key(tid(1), 0, 0),
                StopCause::ProviderError { retryable: false },
            );
            stop.usage = Some(TokenUsage::default());
            stop.cost = Some(CostEstimate {
                usd: 0.5,
                exact: true,
            });
            db.stop_attempt(&lock, stop, now()).unwrap();
        }
        let expected = node_at(&db, 2);
        assert!(matches!(&expected, Node::Step { reply: None, .. }));
        if state == "stopped" {
            let record = serde_json::to_value(&expected).unwrap();
            assert_eq!(record["attempts"][0]["usage"], Value::Null);
            assert_eq!(record["attempts"][0]["cost"], Value::Null);
        }
        let usage = db.begin_read().unwrap().usage(sid(1)).unwrap();
        assert_eq!(usage.turns.len(), 1);
        assert_eq!(usage.path_total.attempts, u64::from(state != "reserved"));
        assert_eq!(
            usage.path_total.input.known,
            (state == "reserved").then_some(0)
        );
        assert_eq!(
            usage.path_total.cost.known,
            (state == "reserved").then_some(CostEstimate {
                usd: 0.0,
                exact: true
            })
        );
        assert_eq!(
            usage.path_total.partial_attempts,
            u64::from(state != "reserved")
        );
        drop(lock);
        drop(db);
        let reopened = SessionStore::open(&layout).unwrap();
        assert_eq!(reopened.begin_read().unwrap().usage(sid(1)).unwrap(), usage);
        assert_eq!(node_at(&reopened, 2), expected, "{state}");
    }
}

#[test]
fn provider_id_and_attempt_source_boundaries() {
    for protocol in [
        AttemptProtocol::ChatCompletions,
        AttemptProtocol::Messages,
        AttemptProtocol::GenerateContent,
        AttemptProtocol::Responses,
        AttemptProtocol::Interactions,
    ] {
        let (_dir, layout, db) = store();
        let lock = db.try_lock_session(sid(1)).unwrap();
        let mut input = accept(sid(1), tid(1));
        input.config.provider = provider("edge-1");
        let origin = AttemptOrigin {
            protocol,
            provider_id: input.config.provider.clone(),
        };
        accept_prepared(&db, &lock, input).unwrap();
        db.start_request(&lock, key(tid(1), 0, 0), origin.clone(), now())
            .unwrap();
        drop(lock);
        drop(db);
        let reopened = SessionStore::open(&layout).unwrap();
        assert_eq!(detail(&reopened).steps[0].attempts[0].origin, origin);
    }
    let (_dir, _layout, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
    let before = records(&db);
    let mut source = origin();
    source.provider_id = provider("other-provider");
    assert!(matches!(
        db.start_request(&lock, key(tid(1), 0, 0), source, now()),
        Err(StoreError::InvalidInput { .. })
    ));
    assert!(before == records(&db));
    db.start_request(&lock, key(tid(1), 0, 0), origin(), now())
        .unwrap();
    let before = records(&db);
    for (source, message_origin) in [
        (
            AttemptOrigin {
                protocol: AttemptProtocol::Messages,
                ..origin()
            },
            Protocol::Messages,
        ),
        (
            AttemptOrigin {
                provider_id: provider("other-provider"),
                ..origin()
            },
            Protocol::ChatCompletions,
        ),
    ] {
        let mut stop = stopped(key(tid(1), 0, 0), StopCause::Cancelled);
        stop.origin = source.clone();
        assert!(db.stop_attempt(&lock, stop, now()).is_err());
        let mut complete = reply(key(tid(1), 0, 0), 0);
        complete.origin = source;
        complete.message.provider_origin = Some(AttemptOrigin {
            protocol: message_origin,
            ..origin()
        });
        assert!(matches!(
            db.close_step(&lock, CloseStep::Finish(complete), now()),
            Err(StoreError::InvalidInput { field: "origin", detail }) if detail == "reply source mismatch"
        ));
        assert!(before == records(&db));
    }
    for source in [
        None,
        Some(AttemptOrigin {
            provider_id: provider("other-provider"),
            ..origin()
        }),
        Some(AttemptOrigin {
            protocol: Protocol::Messages,
            ..origin()
        }),
    ] {
        for count in [0, 1] {
            let mut complete = reply(key(tid(1), 0, 0), count);
            complete.message.provider_origin = source.clone();
            let rejected = if count == 0 {
                db.close_step(&lock, CloseStep::Finish(complete), now())
                    .map(|_| ())
            } else {
                db.register_calls(&lock, complete, &[iid(9)], now())
                    .map(|_| ())
            };
            assert!(matches!(
                rejected,
                Err(StoreError::InvalidInput {
                    field: "origin",
                    ..
                })
            ));
            assert!(before == records(&db));
        }
    }
}

#[test]
fn retries_usage_and_between_request_stops() {
    for cause in [StopCause::Cancelled, StopCause::Deadline] {
        let (_d, _l, db) = store();
        let lock = db.try_lock_session(sid(1)).unwrap();
        accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
        db.start_request(&lock, key(tid(1), 0, 0), origin(), now())
            .unwrap();
        let mut stop = stopped(
            key(tid(1), 0, 0),
            StopCause::ProviderError { retryable: true },
        );
        stop.retry_delay_ms = None;
        let before = records(&db);
        assert!(db.stop_attempt(&lock, stop.clone(), now()).is_err());
        assert!(before == records(&db));
        stop.retry_delay_ms = Some(60000);
        stop.usage = Some(TokenUsage {
            input: Some(1),
            output: Some(2),
            ..Default::default()
        });
        assert!(matches!(
            db.stop_attempt(&lock, stop, now()).unwrap(),
            AttemptStopResult::RetryReady { next_attempt: 1 }
        ));
        let updated = now() + chrono::Duration::seconds(1);
        db.close_step(
            &lock,
            CloseStep::StopBetweenRequests {
                turn_id: tid(1),
                cause: cause.clone(),
            },
            updated,
        )
        .unwrap();
        let t = detail(&db);
        assert_eq!(t.ended_at, updated);
        assert_eq!(
            t.steps[0].end,
            StepEnd::Stop {
                cause: cause.clone()
            }
        );
        assert_eq!(
            t.summary.outcome,
            if cause == StopCause::Cancelled {
                PublicTurnOutcome::Cancelled
            } else {
                PublicTurnOutcome::Failed
            }
        );
        assert!(!t.summary.notice.unwrap().text.contains("provider"));
        assert_eq!(
            node_at(&db, 2).retry_notices().unwrap(),
            vec![RetryNotice {
                attempt: 0,
                delay_ms: 60000,
                notice: Notice {
                    level: NoticeLevel::Warn,
                    text: "old provider diagnostic".into()
                }
            }]
        );
    }
    let (_d, _l, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
    for a in 0..3 {
        db.start_request(&lock, key(tid(1), 0, a), origin(), now())
            .unwrap();
        let result = db
            .stop_attempt(
                &lock,
                stopped(
                    key(tid(1), 0, a),
                    StopCause::ProviderError { retryable: true },
                ),
                now(),
            )
            .unwrap();
        assert_eq!(matches!(result, AttemptStopResult::Closed(_)), a == 2);
    }
    let t = detail(&db);
    assert_eq!(t.steps[0].attempts[2].retry_delay_ms, None);
    assert_eq!(node_at(&db, 2).retry_notices().unwrap().len(), 2);
    assert_eq!(
        *t.usage,
        UsageSummary::from_attempts([(None, UsageCompleteness::Partial, None); 3])
    );
    assert_eq!(
        t.summary.notice.unwrap(),
        Notice {
            level: NoticeLevel::Error,
            text: "old provider diagnostic".into()
        }
    );
    assert!(
        db.start_request(&lock, key(tid(1), 0, 3), origin(), now())
            .is_err()
    );
}
