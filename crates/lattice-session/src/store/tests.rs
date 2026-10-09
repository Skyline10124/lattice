// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::codec::*;
use super::*;
use lancet_protocol::{
    CellEvidenceTriple, CepError, CepErrorCode, CepErrorData, InvocationId, ModelAttemptKey,
    Retriability, SessionId, TurnId,
};
use lattice_conversation::{
    MessagePart, MessageRole, MessageSemantic, NativeMessage, OpaqueProviderState, ProviderOrigin,
    TokenUsage, ToolResultOutcome, UnboundToolCall,
};
use lattice_store::{ProjectStoreKey, StateLayout};
use redb::ReadableTable;
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, os::unix::fs::symlink, sync::Arc};

#[test]
fn structured_result_wire() {
    let wire = json!({
        "invocation_id": "00000000-0000-4000-8000-000000000001",
        "name": "observe.workspace",
        "payload": {"outcome":"succeeded", "payload": {
            "kind":"output_too_large", "digest":"ab".repeat(32), "byte_length":65537
        }}
    });
    let neutral: lattice_conversation::StoredToolResult =
        serde_json::from_value(wire.clone()).unwrap();
    let stored: StoredToolResult = neutral;
    assert_eq!(serde_json::to_value(&stored).unwrap(), wire);
    assert_eq!(stored.payload.outcome(), ToolResultOutcome::Succeeded);
    let digest = lattice_conversation::tool_content_digest("{}")
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    assert_eq!(
        digest,
        "75f9393780337294f4a878b86195ce835e1af90ba8f3a63c2702f6d6e42f29f4"
    );
}

fn now() -> Timestamp {
    "2026-09-26T00:00:00Z".parse().unwrap()
}
fn sid(n: u64) -> SessionId {
    SessionId::parse(&format!("{n:032x}")).unwrap()
}
fn tid(n: u64) -> TurnId {
    TurnId::parse(&format!("00000000-0000-4000-8000-{n:012x}")).unwrap()
}
fn iid(n: u64) -> InvocationId {
    InvocationId::parse(&format!("00000000-0000-4000-8000-{n:012x}")).unwrap()
}
fn project(n: u8) -> ProjectStoreKey {
    ProjectStoreKey::from_canonical_path_bytes(&[b'/', n])
}
fn key(t: TurnId, s: u32, a: u32) -> ModelAttemptKey {
    ModelAttemptKey {
        turn_id: t,
        model_step_ordinal: s,
        attempt_ordinal: a,
    }
}
fn step(t: TurnId, ordinal: u32) -> StepKey {
    StepKey {
        turn_id: t,
        ordinal,
    }
}
fn config() -> FrozenTurnConfig {
    FrozenTurnConfig {
        agent_name: "alpha".into(),
        model: "model".into(),
        provider: "openai".into(),
        system_prompt: None,
        profile_snapshot_json: "{}".into(),
        policy_snapshot_json: "{}".into(),
        catalog: ToolCatalogSnapshot::new(
            "build".into(),
            vec![ToolCatalogEntry {
                name: "observe.workspace".into(),
                descriptor_identity: "a".repeat(64),
                description: "observe".into(),
                input_schema: json!({"type":"object"}),
            }],
        )
        .unwrap(),
        limits: TurnExecutionLimits {
            timeout_secs: 300,
            max_model_steps: 32,
            max_model_attempts: 3,
        },
    }
}
fn accept(s: SessionId, t: TurnId) -> AcceptTurn {
    AcceptTurn {
        session: AcceptSession::New { id: s },
        turn_id: t,
        project_store_key: project(1),
        input: "用户问题🌳".into(),
        config: config(),
        accepted_at: now(),
        agent_selection: AgentSelection::Explicit,
    }
}
fn reply(k: ModelAttemptKey, count: usize) -> CompleteReply {
    let mut parts = vec![MessagePart::semantic_only(MessageSemantic::Text {
        text: "正文🌳".into(),
    })];
    for n in 0..count {
        parts.push(MessagePart {
            semantic: MessageSemantic::ToolCall {
                call: UnboundToolCall {
                    provider_call_id: Some(format!("call-{n}")),
                    name: "observe.workspace".into(),
                    arguments: json!({"path":"private-input"}),
                },
            },
            replay_state: Some(OpaqueProviderState::new("opaque").unwrap()),
        });
        parts.push(MessagePart::semantic_only(MessageSemantic::Reasoning {
            text: format!("推理{n}"),
        }));
    }
    CompleteReply {
        key: k,
        origin: ProviderOrigin::OpenAiCompatible,
        usage: Some(TokenUsage {
            prompt_tokens: 2,
            completion_tokens: 3,
            total_tokens: 5,
        }),
        message: NativeMessage::new(
            MessageRole::Assistant,
            Some(ProviderOrigin::OpenAiCompatible),
            parts,
        ),
    }
}
#[test]
fn rejected_error_round_trip_and_privacy() {
    let (_dir, layout, db) = store();
    let (lock, calls) = running(&db, sid(1), tid(1), 1, 1);
    let binding = &calls.bindings[0];
    let term = terminal(binding, Some(CepErrorCode::CapabilityNotGranted));
    let wire = serde_json::to_value(&term).unwrap();
    assert_eq!(
        wire["result"]["payload"]["payload"]["message"],
        "private-error"
    );
    let bytes = codec::encode(&term).unwrap();
    assert_eq!(
        codec::decode::<InvocationTerminal>("steps", "test", &bytes).unwrap(),
        term
    );
    let mut bad = wire.clone();
    bad["result"]["payload"]["payload"]["unknown"] = json!(true);
    let mut bytes = 1u32.to_be_bytes().to_vec();
    bytes.extend(lancet_protocol::canonical_json(&bad).as_bytes());
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
    db.auto_close(&lock, tid(1), now()).unwrap();
    drop(lock);
    drop(db);
    let db = SessionStore::open(&layout).unwrap();
    let state = durable(&db, sid(1), tid(1));
    let StepState::Closed { invocations, .. } = &state.steps[0].state else {
        panic!("closed");
    };
    assert_eq!(invocations[0].terminal, term);
    let operator = db
        .begin_read()
        .unwrap()
        .operator_status(&project(1))
        .unwrap();
    assert!(
        !serde_json::to_string(&operator)
            .unwrap()
            .contains("private-error")
    );
}

fn stopped(k: ModelAttemptKey, cause: StopCause) -> AttemptStopped {
    AttemptStopped {
        key: k,
        origin: ProviderOrigin::OpenAiCompatible,
        usage: None,
        diagnostic: AttemptDiagnostic {
            provider_error: matches!(cause, StopCause::ProviderError { .. })
                .then(|| "old provider diagnostic".into()),
            ..Default::default()
        },
        cause,
    }
}
fn store() -> (tempfile::TempDir, StateLayout, SessionStore) {
    let dir = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(dir.path());
    let db = SessionStore::create(&layout).unwrap();
    (dir, layout, db)
}
fn running(
    db: &SessionStore,
    s: SessionId,
    t: TurnId,
    count: usize,
    base: u64,
) -> (SessionExecutionLock, RegisteredCalls) {
    let lock = db.try_lock_session(s).unwrap();
    db.accept_turn(&lock, accept(s, t)).unwrap();
    db.start_request(&lock, key(t, 0, 0), now()).unwrap();
    let calls = db
        .register_calls(
            &lock,
            reply(key(t, 0, 0), count),
            &(0..count).map(|n| iid(base + n as u64)).collect::<Vec<_>>(),
            now(),
        )
        .unwrap();
    (lock, calls)
}
fn terminal(
    binding: &lancet_protocol::InvocationBinding,
    code: Option<CepErrorCode>,
) -> InvocationTerminal {
    let receipt = match code {
        None => Ok(json!({"secret-result":42})),
        Some(code) => {
            let mut e = CepError::new(
                CepErrorCode::ExecutionFailed,
                "private-error",
                CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
                    operation: "internal".into(),
                    detail: None,
                }),
                Retriability::Transient,
            );
            e.code = code;
            Err(e)
        }
    };
    prepare_terminal(
        binding,
        receipt,
        CellEvidenceTriple::refused_before_admission(&binding.identity.method),
    )
    .unwrap()
}
fn finish(
    db: &SessionStore,
    lock: &SessionExecutionLock,
    binding: &lancet_protocol::InvocationBinding,
    code: Option<CepErrorCode>,
) {
    let started = db.start_invocation(lock, binding, now()).unwrap();
    assert_eq!(started.binding(), binding);
    db.finish_invocation(
        lock,
        TerminalInput {
            invocation_id: binding.invocation_id,
            terminal: terminal(binding, code),
        },
        now(),
    )
    .unwrap();
}
fn durable(db: &SessionStore, s: SessionId, t: TurnId) -> DurableTurnState {
    db.begin_read().unwrap().durable_turn(s, t).unwrap()
}
fn summary(db: &SessionStore, s: SessionId, t: TurnId) -> TurnSummary {
    db.begin_read()
        .unwrap()
        .turn_detail(s, t)
        .unwrap()
        .summary
        .unwrap()
}
fn filter(p: ProjectStoreKey) -> AuditFilter {
    AuditFilter {
        project_store_key: p,
        session_id: None,
        turn_id: None,
        method: None,
        outcome: None,
    }
}
fn corrupt_json(
    layout: &StateLayout,
    table: redb::TableDefinition<&str, &[u8]>,
    key: &str,
    edit: impl FnOnce(&mut Value),
) {
    let db = redb::Database::open(layout.lattice_root().join("lattice.redb")).unwrap();
    let tx = db.begin_write().unwrap();
    {
        let mut tab = tx.open_table(table).unwrap();
        let bytes = tab.get(key).unwrap().unwrap().value().to_vec();
        let mut v: Value = serde_json::from_slice(&bytes[4..]).unwrap();
        edit(&mut v);
        tab.insert(key, encode(&v).unwrap().as_slice()).unwrap();
    }
    tx.commit().unwrap();
}
fn corrupt_state(layout: &StateLayout, edit: impl FnOnce(&mut Records)) {
    let db = redb::Database::open(layout.lattice_root().join("lattice.redb")).unwrap();
    let tx = db.begin_write().unwrap();
    let mut records = load_write(&tx).unwrap();
    edit(&mut records);
    replace_all_for_test(&tx, &records).unwrap();
    tx.commit().unwrap();
}
// 故障仅改历史，当前槽位、末条投影和索引一致；前态不能被分页/过滤绕过。
#[test]
fn tail_rejects_invalid_lifecycle_across_pages() {
    let mut missed = Vec::new();
    for case in ["repeated_registered", "missing_started", "repeated_started"] {
        let (_dir, layout, db) = store();
        let (lock, calls) = running(&db, sid(1), tid(1), 1, 1);
        let (_other, _) = running(&db, sid(2), tid(2), 1, 2);
        finish(&db, &lock, &calls.bindings[0], None);
        db.auto_close(&lock, tid(1), now()).unwrap();
        let mut selected = filter(project(1));
        selected.session_id = Some(sid(1));
        // 两个调用的 Registered 交错；单条分页应还原同一个调用的前态。
        for after in 0..4 {
            let page = db
                .begin_read()
                .unwrap()
                .tail_audit(&selected, ChangeSeq(after), 1)
                .unwrap();
            assert_eq!(page.next_after, ChangeSeq(after + 1));
            assert_eq!(page.rows.len(), usize::from(after != 1));
        }
        let tx = db.db.begin_write().unwrap();
        let mut records = load_write(&tx).unwrap();
        let (bad_seq, terminal_seq) = match case {
            "repeated_registered" => {
                records.changes.get_mut(&ChangeSeq(3)).unwrap().row.state =
                    OperatorInvocationState::Registered;
                (3, 4)
            }
            "missing_started" => {
                records.changes.remove(&ChangeSeq(3));
                let mut terminal = records.changes.remove(&ChangeSeq(4)).unwrap();
                terminal.sequence = ChangeSeq(3);
                records.changes.insert(ChangeSeq(3), terminal);
                (3, 3)
            }
            _ => {
                let mut terminal = records.changes.remove(&ChangeSeq(4)).unwrap();
                terminal.sequence = ChangeSeq(5);
                records.changes.insert(ChangeSeq(5), terminal);
                let mut started = records.changes[&ChangeSeq(3)].clone();
                started.sequence = ChangeSeq(4);
                records.changes.insert(ChangeSeq(4), started);
                (4, 5)
            }
        };
        records.last_change = ChangeSeq(terminal_seq);
        records.index.get_mut(&iid(1)).unwrap().last_change = ChangeSeq(terminal_seq);
        replace_all_for_test(&tx, &records).unwrap();
        tx.commit().unwrap();
        for (after, limit, outcome) in [
            (0, 100, None),
            (bad_seq - 1, 1, None),
            (terminal_seq - 1, 1, None),
            (0, 100, Some(ToolResultOutcome::Succeeded)),
            (terminal_seq - 1, 1, Some(ToolResultOutcome::Succeeded)),
        ] {
            selected.outcome = outcome;
            let result = db
                .begin_read()
                .unwrap()
                .tail_audit(&selected, ChangeSeq(after), limit);
            if !matches!(result, Err(StoreError::Corruption { .. })) {
                missed.push(format!(
                    "{case}: after={after}, limit={limit}, outcome={outcome:?}"
                ));
            }
        }
        assert!(records == load_read(&db.begin_read().unwrap().tx).unwrap());
        drop(db);
        assert!(matches!(
            SessionStore::open(&layout),
            Err(StoreError::Corruption { .. })
        ));
    }
    assert!(
        missed.is_empty(),
        "tail accepted illegal lifecycle: {missed:?}"
    );
}

// 句柄已打开后注入可解码的坏高水位；另一 Session 的日志不得被登记覆盖。
#[test]
fn registration_rejects_corrupt_change_high_water() {
    for high_water in [0, 4] {
        let (_dir, _layout, db) = store();
        let (first, calls) = running(&db, sid(1), tid(1), 1, 1);
        finish(&db, &first, &calls.bindings[0], None);
        db.auto_close(&first, tid(1), now()).unwrap();
        let lock = db.try_lock_session(sid(2)).unwrap();
        db.accept_turn(&lock, accept(sid(2), tid(2))).unwrap();
        db.start_request(&lock, key(tid(2), 0, 0), now()).unwrap();
        let tx = db.db.begin_write().unwrap();
        tx.open_table(META)
            .unwrap()
            .insert(
                "last_change_seq",
                encode(&ChangeSeq(high_water)).unwrap().as_slice(),
            )
            .unwrap();
        tx.commit().unwrap();
        let before = load_read(&db.begin_read().unwrap().tx).unwrap();
        let result = db.register_calls(&lock, reply(key(tid(2), 0, 0), 1), &[iid(2)], now());
        assert!(
            matches!(result, Err(StoreError::Corruption { .. })),
            "registration accepted corrupt high water {high_water}: {result:?}"
        );
        assert!(before == load_read(&db.begin_read().unwrap().tx).unwrap());
    }
}

// 独立核验发布边界，不能依赖局部装载已经阻止碰撞。
#[test]
fn change_publication_rejects_occupied_sequence() {
    let (_dir, _layout, db) = store();
    let (_lock, _) = running(&db, sid(1), tid(1), 1, 1);
    let original = load_read(&db.begin_read().unwrap().tx).unwrap();
    let mut before = original.clone();
    before.changes.clear();
    let mut after = before.clone();
    let mut conflicting = original.changes[&ChangeSeq(1)].clone();
    conflicting.row.invocation_id = iid(99);
    after.changes.insert(ChangeSeq(1), conflicting);
    let tx = db.db.begin_write().unwrap();
    let result = persist(&tx, &before, &after);
    assert!(
        matches!(result, Err(StoreError::Corruption { .. })),
        "publication accepted occupied sequence: {result:?}"
    );
    assert!(original == load_write(&tx).unwrap());
    drop(tx);
    assert!(original == load_read(&db.begin_read().unwrap().tx).unwrap());
}

// 局部装载不能把直接引用缺失当成“尚未创建”。
#[test]
fn local_reads_reject_broken_references() {
    let mut missed = Vec::new();
    for case in 0..4 {
        let (_dir, _layout, db) = store();
        let (lock, calls) = running(&db, sid(1), tid(1), 1, 1);
        finish(&db, &lock, &calls.bindings[0], None);
        db.auto_close(&lock, tid(1), now()).unwrap();
        let tx = db.db.begin_write().unwrap();
        let mut records = load_write(&tx).unwrap();
        match case {
            0 => records.sessions.get_mut(&sid(1)).unwrap().active_turn = Some(tid(99)),
            1 => records.turns.get_mut(&tid(1)).unwrap().turn_id = tid(99),
            2 => records.index.get_mut(&iid(1)).unwrap().address.part_ordinal = 99,
            _ => {
                records.nodes.remove(&(sid(1), NodeId(2)));
            }
        }
        replace_all_for_test(&tx, &records).unwrap();
        tx.commit().unwrap();
        if !matches!(
            db.begin_read().unwrap().session(sid(1)),
            Err(StoreError::Corruption { .. })
        ) {
            missed.push(case);
        }
        if !matches!(
            db.rewind(&lock, NodeId(0), now()),
            Err(StoreError::Corruption { .. })
        ) {
            missed.push(case);
        }
    }
    assert!(
        missed.is_empty(),
        "local read/write accepted broken references: {missed:?}"
    );
}

// 打开后在自有库注入无关坏行作为读取哨兵；不得触达它，也不得修复它。
#[test]
fn ordinary_operations_are_local() {
    let (_dir, layout, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    db.accept_turn(&lock, accept(sid(1), tid(1))).unwrap();
    db.auto_close(&lock, tid(1), now()).unwrap();
    db.rewind(&lock, NodeId(0), now()).unwrap();
    let mut second = accept(sid(1), tid(2));
    second.session = AcceptSession::Existing { id: sid(1) };
    db.accept_turn(&lock, second).unwrap();
    let other = db.try_lock_session(sid(2)).unwrap();
    db.accept_turn(&other, accept(sid(2), tid(3))).unwrap();
    let tx = db.db.begin_write().unwrap();
    {
        let mut table = tx.open_table(TURNS).unwrap();
        table
            .insert(
                tid(1).to_string().as_str(),
                b"\0\0\0\x01bad unrelated history".as_slice(),
            )
            .unwrap();
        table
            .insert(
                tid(3).to_string().as_str(),
                b"\0\0\0\x01bad unrelated session".as_slice(),
            )
            .unwrap();
    }
    tx.commit().unwrap();
    let read = db.begin_read().unwrap();
    assert_eq!(read.session(sid(1)).unwrap().current, NodeId(2));
    assert_eq!(read.list(None, 1).unwrap().items.len(), 1);
    assert_eq!(read.turn_detail(sid(1), tid(2)).unwrap().steps.len(), 0);
    assert!(read.snapshot(sid(1), None).unwrap().selected_turn.is_some());
    assert!(
        read.node(NodeRef {
            session_id: sid(1),
            node: NodeId(2)
        })
        .is_ok()
    );
    assert!(
        read.history_page(
            &HistoryAnchor {
                session_id: sid(1),
                head: NodeId(2)
            },
            None,
            4096
        )
        .is_ok()
    );
    assert!(matches!(
        read.turn_record(tid(1)),
        Err(StoreError::Corruption { .. })
    ));
    db.start_request(&lock, key(tid(2), 0, 0), now()).unwrap();
    db.close_step(&lock, CloseStep::Finish(reply(key(tid(2), 0, 0), 0)), now())
        .unwrap();
    assert_eq!(
        summary(&db, sid(1), tid(2)).outcome,
        PublicTurnOutcome::Completed
    );
    assert!(matches!(
        db.begin_read().unwrap().log(sid(1)),
        Err(StoreError::Corruption { .. })
    ));
    drop(read);
    drop(other);
    drop(lock);
    drop(db);
    assert!(matches!(
        SessionStore::open(&layout),
        Err(StoreError::Corruption { .. })
    ));
}

// 从合法事实逐一破坏事务专属形状，避免 outcome 不一致掩盖漏洞。
#[test]
fn impossible_transaction_shapes_are_rejected() {
    let mut missed = Vec::new();
    for started in [false, true] {
        let (_dir, layout, db) = store();
        let (lock, calls) = running(&db, sid(1), tid(1), 2, 1);
        if started {
            db.start_invocation(&lock, &calls.bindings[0], now())
                .unwrap();
        }
        db.auto_close(&lock, tid(1), now()).unwrap();
        let mut records = load_read(&db.begin_read().unwrap().tx).unwrap();
        let Node::Step { content, .. } = records.nodes.remove(&(sid(1), NodeId(2))).unwrap() else {
            unreachable!()
        };
        let StepState::Closed { invocations, .. } = &records.steps[&step(tid(1), 0)].state else {
            unreachable!()
        };
        let slots = invocations
            .iter()
            .map(|i| InvocationSlot {
                binding: i.binding.clone(),
                state: SlotState::Terminal(i.terminal.clone()),
            })
            .collect();
        records.steps.get_mut(&step(tid(1), 0)).unwrap().state = StepState::Active {
            active: ActiveStep::Executing {
                reply: content.assistant,
                slots,
            },
        };
        records.turns.get_mut(&tid(1)).unwrap().state = TurnState::Running;
        let session = records.sessions.get_mut(&sid(1)).unwrap();
        session.current = NodeId(1);
        session.active_turn = Some(tid(1));
        let label = if started {
            "executing synthetic indeterminate"
        } else {
            "executing not-executed"
        };
        if !matches!(
            plan_auto_close(&records.durable(sid(1), tid(1)).unwrap(), now()),
            Err(StoreError::Corruption { .. })
        ) {
            missed.push(label);
        }
        drop(lock);
        drop(db);
        corrupt_state(&layout, |r| *r = records);
        if !matches!(
            SessionStore::open(&layout),
            Err(StoreError::Corruption { .. })
        ) {
            missed.push(label);
        }
    }
    let (_dir, _layout, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    db.accept_turn(&lock, accept(sid(1), tid(1))).unwrap();
    db.start_request(&lock, key(tid(1), 0, 0), now()).unwrap();
    db.stop_attempt(
        &lock,
        stopped(
            key(tid(1), 0, 0),
            StopCause::ProviderError { retryable: true },
        ),
        now(),
    )
    .unwrap();
    let retry = durable(&db, sid(1), tid(1));
    for case in 0..7 {
        let mut state = retry.clone();
        match case {
            0 => {
                state.steps[0].state = StepState::Closed {
                    end: StepEnd::Stop {
                        node: None,
                        cause: StopCause::ProviderError { retryable: true },
                    },
                    invocations: vec![],
                };
                let (outcome, notice) = close::outcome(&state.steps[0]).unwrap();
                state.turn.state = TurnState::Closed {
                    outcome,
                    notice,
                    usage: close::usage(&state.steps),
                    ended_at: now(),
                };
                state.session.active_turn = None;
            }
            1 => {
                state.turn.config.limits.max_model_attempts = 1;
                state.turn.config_digest = state.turn.config.digest().unwrap();
            }
            2 => state.steps[0].attempts.clear(),
            3 => {
                if let AttemptEnd::Stopped { cause, .. } = &mut state.steps[0].attempts[0].end {
                    *cause = StopCause::ProviderError { retryable: false };
                }
            }
            4 => {
                state.steps[0].state = StepState::Active {
                    active: ActiveStep::Requesting { attempt: 0 },
                }
            }
            5 => {
                state.steps[0].state = StepState::Active {
                    active: ActiveStep::Executing {
                        reply: NativeMessage::new(MessageRole::Assistant, None, vec![]),
                        slots: vec![],
                    },
                }
            }
            _ => {
                state.steps[0].state = StepState::Closed {
                    end: StepEnd::Stop {
                        node: None,
                        cause: StopCause::ProcessExit,
                    },
                    invocations: vec![],
                }
            }
        }
        if !matches!(
            plan_auto_close(&state, now()),
            Err(StoreError::Corruption { .. })
        ) {
            missed.push(if case == 0 {
                "retryable provider closed before exhaustion"
            } else {
                "invalid active retry shape"
            });
        }
    }
    drop(lock);
    let (lock, calls) = running(&db, sid(2), tid(2), 3, 10);
    db.start_invocation(&lock, &calls.bindings[0], now())
        .unwrap();
    db.auto_close(&lock, tid(2), now()).unwrap();
    let closed = durable(&db, sid(2), tid(2));
    // 关闭后槽位为 [Indeterminate(NoResult), NotExecuted, NotExecuted]；以下形状都不可能由关闭事务写出。
    for (index, payload, reason, label) in [
        (
            1,
            ResultPayload::Indeterminate(IndeterminateCause::NoResult),
            EvidenceUnavailableReason::NoResult,
            "no-result after a stopping slot",
        ),
        (
            0,
            ResultPayload::Indeterminate(IndeterminateCause::Reported),
            EvidenceUnavailableReason::NoResult,
            "reported indeterminate without receipt",
        ),
        (
            1,
            ResultPayload::NotExecuted,
            EvidenceUnavailableReason::NoResult,
            "not-executed with no-result evidence",
        ),
    ] {
        let mut state = closed.clone();
        let StepState::Closed { invocations, .. } = &mut state.steps[0].state else {
            unreachable!()
        };
        invocations[index].terminal.result.payload = payload;
        invocations[index].terminal.evidence = InvocationEvidence::Unavailable { reason };
        if !matches!(
            plan_auto_close(&state, now()),
            Err(StoreError::Corruption { .. })
        ) {
            missed.push(label);
        }
    }
    assert!(
        missed.is_empty(),
        "accepted impossible transaction shapes: {missed:?}"
    );
}

#[test]
fn json_payload_field_names_are_not_store_records() {
    let (_dir, layout, db) = store();
    let (lock, calls) = running(&db, sid(1), tid(1), 1, 1);
    let value = json!({"evidence":{"kind":"recorded","value":{"custom":true},"custom":true}});
    db.start_invocation(&lock, &calls.bindings[0], now())
        .unwrap();
    let terminal = prepare_terminal(
        &calls.bindings[0],
        Ok(value.clone()),
        CellEvidenceTriple::refused_before_admission("observe.workspace"),
    )
    .unwrap();
    db.finish_invocation(
        &lock,
        TerminalInput {
            invocation_id: iid(1),
            terminal,
        },
        now(),
    )
    .unwrap();
    db.auto_close(&lock, tid(1), now()).unwrap();
    drop(lock);
    drop(db);
    let db = SessionStore::open(&layout).unwrap();
    let Node::Step { content, .. } = db
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
        matches!(&content.results[0].payload, ResultPayload::Succeeded(SuccessContent::Json {value:actual, ..}) if *actual == value)
    );
}

// 持久边界必须发现复用类型丢弃的嵌套字段；任意 JSON 内容和 evidence 扩展不受此限。
#[test]
fn nested_unknown_fields_are_rejected() {
    let mut missed = Vec::new();
    for pointer in [
        "/config",
        "/config/limits",
        "/config/catalog",
        "/config/catalog/entries/0",
    ] {
        let (_dir, layout, db) = store();
        let lock = db.try_lock_session(sid(1)).unwrap();
        db.accept_turn(&lock, accept(sid(1), tid(1))).unwrap();
        drop(lock);
        drop(db);
        corrupt_json(&layout, TURNS, &tid(1).to_string(), |v| {
            v.pointer_mut(pointer).unwrap()["unknown_new_policy"] = json!("private");
        });
        if !matches!(
            SessionStore::open(&layout),
            Err(StoreError::Corruption { .. })
        ) {
            missed.push(pointer);
        }
    }
    let (_dir, _layout, db) = store();
    let (lock, calls) = running(&db, sid(1), tid(1), 1, 1);
    finish(
        &db,
        &lock,
        &calls.bindings[0],
        Some(CepErrorCode::ExecutionFailed),
    );
    let record = durable(&db, sid(1), tid(1)).steps.remove(0);
    let original = serde_json::to_value(&record).unwrap();
    for pointer in [
        "/attempts/0/usage",
        "/attempts/0/end/step",
        "/state/active/reply",
        "/state/active/reply/parts/1",
        "/state/active/reply/parts/1/semantic/tool_call/call",
        "/state/active/slots/0/binding",
        "/state/active/slots/0/binding/identity",
        "/state/active/slots/0/state/terminal/result/payload/payload",
        "/state/active/slots/0/state/terminal/evidence",
        "/state/active/slots/0/state/terminal/evidence/value",
    ] {
        let mut v = original.clone();
        let target = v
            .pointer_mut(pointer)
            .unwrap_or_else(|| panic!("fixture pointer {pointer}: {original}"));
        target["unknown_new_policy"] = json!("private");
        if !matches!(
            decode::<StepRecord>("steps", "test", &encode(&v).unwrap()),
            Err(StoreError::Corruption { .. })
        ) {
            missed.push(pointer);
        }
    }
    let mut extended = original;
    extended
        .pointer_mut("/state/active/slots/0/state/terminal/evidence/value/admission")
        .unwrap()["future"] = json!({"nested":true});
    assert!(decode::<StepRecord>("steps", "test", &encode(&extended).unwrap()).is_ok());
    let effect = json!({"seq":0,"cell":{"ctx_op":"alloc","ext_impact":"none","scope":"workspace"},"scope":{"scope":"workspace","trust":"in","channel":"workspace"},"target":{"kind":"path","path":"a"},"source":"operation","future":true});
    extended
        .pointer_mut("/state/active/slots/0/state/terminal/evidence/value/effects/effects")
        .unwrap()
        .as_array_mut()
        .unwrap()
        .push(effect);
    assert!(decode::<StepRecord>("steps", "test", &encode(&extended).unwrap()).is_ok());
    extended
        .pointer_mut("/state/active/slots/0/state/terminal/evidence/value/effects/effects/0/cell")
        .unwrap()["unknown"] = json!(true);
    assert!(matches!(
        decode::<StepRecord>("steps", "test", &encode(&extended).unwrap()),
        Err(StoreError::Corruption { .. })
    ));
    assert!(missed.is_empty(), "silently ignored fields at {missed:?}");
}

// 独立 JSON 合法不代表最终持久 envelope 可读；拒绝必须先于提交。
#[test]
fn deep_arguments_are_rejected_before_commit() {
    let (_dir, layout, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    db.accept_turn(&lock, accept(sid(1), tid(1))).unwrap();
    db.start_request(&lock, key(tid(1), 0, 0), now()).unwrap();
    let before = load_read(&db.begin_read().unwrap().tx).unwrap();
    let mut input = reply(key(tid(1), 0, 0), 1);
    let json = format!("{}0{}", "{\"a\":".repeat(120), "}".repeat(120));
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
    db.auto_close(&lock, tid(1), now()).unwrap();
    drop(lock);
    drop(db);
    let reopened = SessionStore::open(&layout).unwrap();
    let Node::Step { content, .. } = reopened
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
    assert_eq!(
        content.assistant.tool_calls().next().unwrap().1.arguments,
        value
    );
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
        let terminal = prepare_terminal(
            &calls.bindings[0],
            Ok(value.clone()),
            CellEvidenceTriple::refused_before_admission("observe.workspace"),
        )
        .unwrap();
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
            let Node::Step { content, .. } = db
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
                matches!(&content.results[0].payload, ResultPayload::Succeeded(SuccessContent::Json { value: actual, .. }) if *actual == value)
            );
        }
    }
    drop(lock);
    drop(db);
    SessionStore::open(&layout).unwrap();
}

// 固定长度键、版本与错误定位，不接受宽松 UUID 或泄漏载荷。
#[test]
fn keys_and_versions() {
    for n in [0, 1, 255, u64::MAX] {
        let key = node_key(sid(1), NodeId(n));
        assert_eq!(decode_node_key(&key).unwrap(), (sid(1), NodeId(n)));
    }
    assert!(node_key(sid(1), NodeId(255)) < node_key(sid(1), NodeId(256)));
    for n in [0, 1, u32::MAX] {
        let k = step(tid(1), n);
        assert_eq!(decode_step_key(&step_key(k)).unwrap(), k);
    }
    for bytes in [vec![], vec![b'a'; 39], vec![b'a'; 41], vec![b'A'; 40]] {
        assert!(decode_node_key(&bytes).is_err());
        assert!(decode_step_key(&bytes).is_err());
    }
    let mut bad = step_key(step(tid(1), 0));
    bad[14] = b'1';
    assert!(decode_step_key(&bad).is_err());
    let encoded = encode(&Node::Root).unwrap();
    assert_eq!(decode::<Node>("nodes", "0", &encoded).unwrap(), Node::Root);
    for bytes in [
        vec![],
        vec![0, 0, 0],
        vec![0, 0, 0, 2, b'0'],
        vec![0, 0, 0, 1, 255],
        b"\0\0\0\x01{\"secret\":true}".to_vec(),
        b"\0\0\0\x01\"not_a_node\"".to_vec(),
    ] {
        let error = decode::<Node>("nodes", "0", &bytes).unwrap_err();
        assert!(!format!("{error:?}").contains("secret"));
    }
    let mut v = serde_json::to_value(accept(sid(1), tid(1)).config).unwrap();
    v["limits"]["max_model_attempts"] = json!(12);
    assert!(validate::config(&serde_json::from_value(v).unwrap()).is_err());
}

// 全局旧库拒绝优先于创建，损坏目标不可覆盖。
#[test]
fn creation_and_legacy() {
    let dir = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(dir.path());
    assert!(matches!(
        SessionStore::open(&layout),
        Err(StoreError::NotInitialized { .. })
    ));
    assert!(!layout.lattice_root().exists());
    fs::create_dir_all(layout.project_dir(&project(9))).unwrap();
    let old = layout.lattice_root().join("sessions.git");
    fs::write(&old, b"old").unwrap();
    let inv = layout.project_store(&project(9), "invocations.git");
    symlink("missing", &inv).unwrap();
    for create in [false, true] {
        let result = if create {
            SessionStore::create(&layout)
        } else {
            SessionStore::open(&layout)
        };
        match result {
            Err(StoreError::LegacyStores { paths }) => {
                assert_eq!(paths.len(), 2);
                assert!(paths.contains(&old));
                assert!(paths.contains(&inv));
            }
            _ => panic!("expected legacy refusal"),
        }
        assert!(!layout.lattice_root().join("lattice.redb").exists());
        assert_eq!(fs::read(&old).unwrap(), b"old");
    }
    fs::remove_file(&old).unwrap();
    fs::remove_file(&inv).unwrap();
    for kind in 0..4 {
        let path = layout.lattice_root().join("lattice.redb");
        match kind {
            0 => fs::write(&path, []).unwrap(),
            1 => fs::write(&path, b"random private payload").unwrap(),
            2 => fs::create_dir(&path).unwrap(),
            _ => symlink("missing", &path).unwrap(),
        };
        assert!(SessionStore::open(&layout).is_err());
        assert!(SessionStore::create(&layout).is_err());
        if kind == 2 {
            fs::remove_dir(path).unwrap();
        } else {
            fs::remove_file(path).unwrap();
        }
    }
    fs::remove_dir_all(layout.projects_dir()).unwrap();
    fs::write(layout.projects_dir(), b"not a directory").unwrap();
    assert!(matches!(
        SessionStore::open(&layout),
        Err(StoreError::Io { .. }) | Err(StoreError::Corruption { .. })
    ));
    fs::remove_file(layout.projects_dir()).unwrap();
    let db = SessionStore::create(&layout).unwrap();
    assert!(
        db.begin_read()
            .unwrap()
            .list(None, 10)
            .unwrap()
            .items
            .is_empty()
    );
}

// 注入发布阶段故障，不模拟 redb 的内部事务原子性。
#[test]
fn creation_failures() {
    for point in [
        FaultPoint::Probe,
        FaultPoint::CreateCommit,
        FaultPoint::BeforeRename,
        FaultPoint::AfterRename,
        FaultPoint::Cleanup,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let layout = StateLayout::in_state_root(dir.path());
        fs::create_dir_all(layout.lattice_root()).unwrap();
        if point == FaultPoint::Cleanup {
            fs::write(
                layout
                    .lattice_root()
                    .join(".lattice.redb-create-00000000-0000-4000-8000-000000000001"),
                b"stale",
            )
            .unwrap();
        }
        let result = with_fault(point, || SessionStore::create(&layout));
        assert!(result.is_err(), "{point:?}");
        assert_eq!(
            layout.lattice_root().join("lattice.redb").exists(),
            point == FaultPoint::AfterRename
        );
        let db = SessionStore::create(&layout).unwrap();
        let lock = db.try_lock_session(sid(1)).unwrap();
        db.accept_turn(&lock, accept(sid(1), tid(1))).unwrap();
        drop(lock);
        drop(db);
        let db = SessionStore::create(&layout).unwrap();
        assert!(
            db.begin_read()
                .unwrap()
                .turn_record(tid(1))
                .unwrap()
                .is_some()
        );
    }
    let dir = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(dir.path());
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..2)
            .map(|_| {
                scope.spawn(|| match SessionStore::create(&layout) {
                    Ok(db) => {
                        drop(db);
                        true
                    }
                    Err(StoreError::Database(redb::DatabaseError::DatabaseAlreadyOpen)) => false,
                    Err(e) => panic!("{e:?}"),
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(results.contains(&true));
    });
    assert!(SessionStore::open(&layout).is_ok());
}

// 找回先于新配置校验；每个非法接受均保持空库。
#[test]
fn acceptance_identity_and_limits() {
    let (_dir, _layout, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    let invalid: Vec<AcceptTurn> = (0..9)
        .map(|case| {
            let mut a = accept(sid(1), tid(1));
            match case {
                0 => a.input = "x".repeat(MAX_USER_INPUT_BYTES + 1),
                1 => a.agent_selection = AgentSelection::Inherited,
                2 => a.config.profile_snapshot_json = "{\"api_key\":\"private\"}".into(),
                3 => a.config.policy_snapshot_json = "{ }".into(),
                4 => a
                    .config
                    .catalog
                    .entries
                    .push(a.config.catalog.entries[0].clone()),
                5 => a.config.limits.max_model_steps = 33,
                6 => a.config.limits.max_model_attempts = 0,
                7 => a.config.agent_name = String::new(),
                _ => a.config.catalog.digest = [0; 32],
            }
            a
        })
        .collect();
    for a in invalid {
        assert!(db.accept_turn(&lock, a).is_err());
        assert!(
            db.begin_read()
                .unwrap()
                .list(None, 10)
                .unwrap()
                .items
                .is_empty()
        );
    }
    let mut a = accept(sid(1), tid(1));
    a.input = "x".repeat(MAX_USER_INPUT_BYTES);
    db.accept_turn(&lock, a.clone()).unwrap();
    let lock2 = db.try_lock_session(sid(2)).unwrap();
    let mut retry = a.clone();
    retry.session = AcceptSession::New { id: sid(2) };
    retry.config.agent_name = String::new();
    assert!(
        matches!(db.accept_turn(&lock2,retry).unwrap(),AcceptedTurn::Existing(t) if t.session_id==sid(1))
    );
    for case in 0..3 {
        let mut bad = a.clone();
        match case {
            0 => bad.input.push('!'),
            1 => bad.project_store_key = project(2),
            _ => bad.session = AcceptSession::Existing { id: sid(2) },
        };
        assert!(matches!(
            db.accept_turn(if case == 2 { &lock2 } else { &lock }, bad),
            Err(StoreError::Conflict { .. })
        ));
    }
    let mut busy = accept(sid(1), tid(2));
    busy.session = AcceptSession::Existing { id: sid(1) };
    assert!(matches!(
        db.accept_turn(&lock, busy),
        Err(StoreError::RecoveryRequired { .. })
    ));
    assert_eq!(
        db.begin_read().unwrap().list(None, 10).unwrap().items.len(),
        1
    );
    assert!(matches!(
        db.accept_turn(&lock, accept(sid(1), tid(3))),
        Err(StoreError::SessionCollision { .. })
    ));
}

// 通过真实领域事务分配极值；只压缩测试库的序号空间，不替换分配器。
#[test]
fn sequence_exhaustion_aborts_transactions() {
    for case in ["node", "change_at_max", "change_in_batch"] {
        let (_dir, _layout, db) = store();
        let lock = db.try_lock_session(sid(1)).unwrap();
        db.accept_turn(&lock, accept(sid(1), tid(1))).unwrap();
        if case == "node" {
            db.auto_close(&lock, tid(1), now()).unwrap();
            let tx = db.db.begin_write().unwrap();
            let mut records = load_write(&tx).unwrap();
            let node = records.nodes.remove(&(sid(1), NodeId(1))).unwrap();
            records.nodes.insert((sid(1), NodeId(u64::MAX)), node);
            records.sessions.get_mut(&sid(1)).unwrap().current = NodeId(u64::MAX);
            records.turns.get_mut(&tid(1)).unwrap().user_node = NodeId(u64::MAX);
            replace_all_for_test(&tx, &records).unwrap();
            tx.commit().unwrap();
        } else {
            db.start_request(&lock, key(tid(1), 0, 0), now()).unwrap();
            let (_other, _) = running(&db, sid(2), tid(2), 1, 99);
            let tx = db.db.begin_write().unwrap();
            let mut records = load_write(&tx).unwrap();
            let high_water = ChangeSeq(if case == "change_in_batch" {
                u64::MAX - 1
            } else {
                u64::MAX
            });
            let mut change = records.changes.remove(&ChangeSeq(1)).unwrap();
            change.sequence = high_water;
            records.changes.insert(high_water, change);
            records.index.get_mut(&iid(99)).unwrap().last_change = high_water;
            records.last_change = high_water;
            replace_all_for_test(&tx, &records).unwrap();
            tx.commit().unwrap();
        }
        let before = load_read(&db.begin_read().unwrap().tx).unwrap();
        let reached_commit = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let probe = reached_commit.clone();
        let result = with_write_probe(
            move |stage| {
                if stage == WriteStage::BeforeCommit {
                    probe.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            },
            || {
                if case == "node" {
                    let mut input = accept(sid(1), tid(3));
                    input.session = AcceptSession::Existing { id: sid(1) };
                    db.accept_turn(&lock, input).map(|_| ())
                } else {
                    // MAX-1 时首项可分配 MAX，第二项才耗尽；不得留下首项索引/日志。
                    db.register_calls(&lock, reply(key(tid(1), 0, 0), 2), &[iid(1), iid(2)], now())
                        .map(|_| ())
                }
            },
        );
        let expected_kind = if case == "node" { "node" } else { "change" };
        assert!(
            matches!(result, Err(StoreError::SequenceExhausted { kind }) if kind == expected_kind),
            "{case}: actual allocator did not reject exhaustion: {result:?}"
        );
        assert!(!reached_commit.load(std::sync::atomic::Ordering::SeqCst));
        assert!(
            before == load_read(&db.begin_read().unwrap().tx).unwrap(),
            "{case}: database changed"
        );
    }
}

// 锁能力与读取不取操作锁。
#[test]
fn sequence_and_lock_boundaries() {
    let (_dir, _layout, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    assert!(matches!(
        db.try_lock_session(sid(1)),
        Err(StoreError::Busy { .. })
    ));
    let other = db.try_lock_session(sid(2)).unwrap();
    assert!(matches!(
        db.accept_turn(&other, accept(sid(1), tid(1))),
        Err(StoreError::InvalidLockCapability)
    ));
    let (_d2, _l2, db2) = store();
    assert!(matches!(
        db2.accept_turn(&lock, accept(sid(1), tid(1))),
        Err(StoreError::InvalidLockCapability)
    ));
    db.accept_turn(&lock, accept(sid(1), tid(1))).unwrap();
    db.auto_close(&lock, tid(1), now()).unwrap();
    let view = db.begin_read().unwrap();
    assert_eq!(view.session(sid(1)).unwrap().current, NodeId(1));
}

// 预算、退避停止与 usage 来源组合。
#[test]
fn attempts_and_step_budget() {
    let (_dir, _layout, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    db.accept_turn(&lock, accept(sid(1), tid(1))).unwrap();
    for k in [key(tid(1), 1, 0), key(tid(1), 0, 1)] {
        assert!(db.start_request(&lock, k, now()).is_err());
    }
    db.start_request(&lock, key(tid(1), 0, 0), now()).unwrap();
    assert!(db.start_request(&lock, key(tid(1), 0, 0), now()).is_err());
    assert!(
        db.stop_attempt(
            &lock,
            stopped(key(tid(1), 0, 1), StopCause::Cancelled),
            now()
        )
        .is_err()
    );
    assert!(
        db.stop_attempt(
            &lock,
            stopped(key(tid(1), 0, 0), StopCause::ModelStepLimit),
            now()
        )
        .is_err()
    );
    assert!(matches!(
        db.stop_attempt(
            &lock,
            stopped(
                key(tid(1), 0, 0),
                StopCause::ProviderError { retryable: true }
            ),
            now()
        )
        .unwrap(),
        AttemptStopResult::RetryReady { next_attempt: 1 }
    ));
    db.close_step(
        &lock,
        CloseStep::StopBetweenRequests {
            turn_id: tid(1),
            cause: StopCause::Deadline,
        },
        now(),
    )
    .unwrap();
    let detail = db
        .begin_read()
        .unwrap()
        .turn_detail(sid(1), tid(1))
        .unwrap();
    assert_eq!(
        detail.summary.unwrap().notice.unwrap().text,
        "turn deadline exceeded"
    );
    assert!(matches!(
        detail.usage,
        UsageSummary::Partial {
            unknown_attempts: 1,
            ..
        }
    ));
    assert!(db.start_request(&lock, key(tid(1), 1, 0), now()).is_err());
    drop(lock);
    for (index, cause) in [
        StopCause::Cancelled,
        StopCause::OutputLimit,
        StopCause::InvalidBatch,
        StopCause::ProviderError { retryable: false },
        StopCause::ProviderError { retryable: true },
    ]
    .into_iter()
    .enumerate()
    {
        let s = sid(10 + index as u64);
        let t = tid(10 + index as u64);
        let lock = db.try_lock_session(s).unwrap();
        let mut a = accept(s, t);
        a.config.limits.max_model_attempts = 1;
        db.accept_turn(&lock, a).unwrap();
        db.start_request(&lock, key(t, 0, 0), now()).unwrap();
        assert!(matches!(
            db.stop_attempt(&lock, stopped(key(t, 0, 0), cause.clone()), now())
                .unwrap(),
            AttemptStopResult::Closed(_)
        ));
        let sum = summary(&db, s, t);
        assert_eq!(
            sum.outcome,
            if cause == StopCause::Cancelled {
                PublicTurnOutcome::Cancelled
            } else {
                PublicTurnOutcome::Failed
            }
        );
        assert_eq!(db.begin_read().unwrap().log(s).unwrap().nodes.len(), 2);
    }
    let lock = db.try_lock_session(sid(100)).unwrap();
    let mut a = accept(sid(100), tid(100));
    a.config.limits.max_model_attempts = 11;
    db.accept_turn(&lock, a).unwrap();
    assert!(
        db.close_step(
            &lock,
            CloseStep::StopBetweenRequests {
                turn_id: tid(100),
                cause: StopCause::ModelStepLimit
            },
            now()
        )
        .is_err()
    );
    for s in 0..32 {
        for attempt in 0..11 {
            db.start_request(&lock, key(tid(100), s, attempt), now())
                .unwrap();
            if attempt < 10 {
                let mut stop = stopped(
                    key(tid(100), s, attempt),
                    StopCause::ProviderError { retryable: true },
                );
                stop.usage = Some(TokenUsage {
                    prompt_tokens: u64::MAX,
                    completion_tokens: 0,
                    total_tokens: u64::MAX,
                });
                db.stop_attempt(&lock, stop, now()).unwrap();
            }
        }
        let calls = db
            .register_calls(
                &lock,
                reply(key(tid(100), s, 10), 1),
                &[iid(1000 + u64::from(s))],
                now(),
            )
            .unwrap();
        finish(&db, &lock, &calls.bindings[0], None);
        db.close_step(
            &lock,
            CloseStep::Continue {
                key: step(tid(100), s),
            },
            now(),
        )
        .unwrap();
    }
    assert!(
        db.start_request(&lock, key(tid(100), 32, 0), now())
            .is_err()
    );
    db.close_step(
        &lock,
        CloseStep::StopBetweenRequests {
            turn_id: tid(100),
            cause: StopCause::ModelStepLimit,
        },
        now(),
    )
    .unwrap();
    let d = db
        .begin_read()
        .unwrap()
        .turn_detail(sid(100), tid(100))
        .unwrap();
    assert_eq!(d.steps.len(), 33);
    assert!(d.steps[32].attempts.is_empty());
    assert!(matches!(d.usage,UsageSummary::Complete { total } if total.total_tokens==u64::MAX));
}

// 失败整批回滚、冲突重试、顺序与配对。
#[test]
fn registration_and_ordering() {
    let (_dir, layout, db) = store();
    let (lock, calls) = running(&db, sid(1), tid(1), 3, 1);
    assert!(db.start_request(&lock, key(tid(1), 0, 1), now()).is_err());
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
    assert!(
        db.start_invocation(&lock, &calls.bindings[1], now())
            .is_err()
    );
    let mut fake = calls.bindings[0].clone();
    fake.identity.arguments_digest = [0; 32];
    assert!(db.start_invocation(&lock, &fake, now()).is_err());
    assert!(
        db.finish_invocation(
            &lock,
            TerminalInput {
                invocation_id: iid(1),
                terminal: terminal(&calls.bindings[0], None)
            },
            now()
        )
        .is_err()
    );
    finish(
        &db,
        &lock,
        &calls.bindings[0],
        Some(CepErrorCode::ExecutionFailed),
    );
    assert!(
        db.start_invocation(&lock, &calls.bindings[0], now())
            .is_err()
    );
    assert!(
        db.finish_invocation(
            &lock,
            TerminalInput {
                invocation_id: iid(1),
                terminal: terminal(&calls.bindings[0], None)
            },
            now()
        )
        .is_err()
    );
    finish(
        &db,
        &lock,
        &calls.bindings[1],
        Some(CepErrorCode::GuardDenied),
    );
    finish(&db, &lock, &calls.bindings[2], None);
    db.close_step(
        &lock,
        CloseStep::Continue {
            key: step(tid(1), 0),
        },
        now(),
    )
    .unwrap();
    db.start_request(&lock, key(tid(1), 1, 0), now()).unwrap();
    let before = db
        .begin_read()
        .unwrap()
        .tail_audit(&filter(project(1)), ChangeSeq(0), 100)
        .unwrap();
    for case in 0..8 {
        let mut rep = reply(key(tid(1), 1, 0), 2);
        let mut ids = vec![iid(4), iid(5)];
        match case {
            0 => rep = reply(key(tid(1), 1, 0), 0),
            1 => rep.message.role = MessageRole::User,
            2 => {
                if let MessageSemantic::ToolCall { call } = &mut rep.message.parts[1].semantic {
                    call.arguments = json!([]);
                }
            }
            3 => {
                if let MessageSemantic::ToolCall { call } = &mut rep.message.parts[3].semantic {
                    call.provider_call_id = Some("call-0".into());
                }
            }
            4 => rep = reply(key(tid(1), 1, 0), 33),
            5 => {
                if let MessageSemantic::ToolCall { call } = &mut rep.message.parts[1].semantic {
                    call.name = "unknown".into();
                }
            }
            6 => ids = vec![iid(4), iid(4)],
            _ => ids = vec![iid(4), iid(1)],
        }
        assert!(db.register_calls(&lock, rep, &ids, now()).is_err());
        assert_eq!(
            db.begin_read()
                .unwrap()
                .tail_audit(&filter(project(1)), ChangeSeq(0), 100)
                .unwrap(),
            before
        );
    }
    let rep = reply(key(tid(1), 1, 0), 2);
    let calls2 = db
        .register_calls(&lock, rep.clone(), &[iid(4), iid(5)], now())
        .unwrap();
    assert_eq!(calls2.reply.parts.len(), rep.message.parts.len());
    assert_eq!(
        calls2.reply.parts[1].replay_state,
        rep.message.parts[1].replay_state
    );
    assert_eq!(calls2.bindings[1].identity.part_ordinal, 3);
    assert!(
        db.close_step(&lock, CloseStep::Finish(reply(key(tid(1), 1, 0), 0)), now())
            .is_err()
    );
    db.auto_close(&lock, tid(1), now()).unwrap();
    drop(lock);
    drop(db);
    let db = SessionStore::open(&layout).unwrap();
    let log = db.begin_read().unwrap().log(sid(1)).unwrap();
    assert_eq!(log.nodes.len(), 4);
    let node = db
        .begin_read()
        .unwrap()
        .node(NodeRef {
            session_id: sid(1),
            node: NodeId(3),
        })
        .unwrap();
    assert!(matches!(node,Node::Step { content,.. } if content.results.len()==2));
}

// 逐行闭合；非法持久终态不能顺手修复。
#[test]
fn auto_close_matrix() {
    for case in 0..8 {
        let (_dir, layout, db) = store();
        let lock = db.try_lock_session(sid(1)).unwrap();
        db.accept_turn(&lock, accept(sid(1), tid(1))).unwrap();
        if case > 0 {
            db.start_request(&lock, key(tid(1), 0, 0), now()).unwrap();
        }
        if case == 2 {
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
        if case >= 3 {
            let calls = db
                .register_calls(
                    &lock,
                    reply(key(tid(1), 0, 0), 3),
                    &[iid(1), iid(2), iid(3)],
                    now(),
                )
                .unwrap();
            if case >= 4 {
                finish(&db, &lock, &calls.bindings[0], None);
            }
            if case == 4 {
                db.start_invocation(&lock, &calls.bindings[1], now())
                    .unwrap();
            }
            if case >= 5 {
                for binding in &calls.bindings[1..] {
                    finish(
                        &db,
                        &lock,
                        binding,
                        if case == 7 {
                            Some(CepErrorCode::CallCancelled)
                        } else {
                            None
                        },
                    );
                    if case == 7 {
                        break;
                    }
                }
            }
            if case == 6 {
                db.close_step(
                    &lock,
                    CloseStep::Continue {
                        key: step(tid(1), 0),
                    },
                    now(),
                )
                .unwrap();
            }
        }
        let state = durable(&db, sid(1), tid(1));
        let copy = state.clone();
        let plan = plan_auto_close(&state, now()).unwrap();
        assert_eq!(plan, plan_auto_close(&state, now()).unwrap());
        assert_eq!(state, copy);
        let before_records = load_read(&db.begin_read().unwrap().tx).unwrap();
        db.auto_close(&lock, tid(1), now()).unwrap();
        let d = db
            .begin_read()
            .unwrap()
            .turn_detail(sid(1), tid(1))
            .unwrap();
        assert_eq!(
            d.summary.unwrap().outcome,
            if case == 7 {
                PublicTurnOutcome::Cancelled
            } else {
                PublicTurnOutcome::Failed
            }
        );
        assert_eq!(
            d.steps.last().unwrap().attempts.len(),
            if case == 0 || case == 6 { 0 } else { 1 }
        );
        let first = durable(&db, sid(1), tid(1));
        // 崩溃落在 Started 前后都是进程退出；只有已写入的槽位自身停止（此处是取消）优先。
        let expected_cause = match case {
            7 => StopCause::Cancelled,
            _ => StopCause::ProcessExit,
        };
        let final_step = first.steps.last().unwrap();
        let StepState::Closed { end, invocations } = &final_step.state else {
            panic!("case {case}: active after close")
        };
        assert_eq!(
            *end,
            StepEnd::Stop {
                node: if (3..=7).contains(&case) && case != 6 {
                    Some(NodeId(2))
                } else {
                    None
                },
                cause: expected_cause
            },
            "case {case}"
        );
        let mut expected_attempts = state
            .steps
            .last()
            .map(|s| s.attempts.clone())
            .unwrap_or_default();
        if case == 1 {
            expected_attempts.push(AttemptRecord {
                attempt: 0,
                origin: ProviderOrigin::OpenAiCompatible,
                usage: None,
                end: AttemptEnd::Stopped {
                    cause: StopCause::ProcessExit,
                    diagnostic: AttemptDiagnostic::default(),
                },
            });
        }
        if case == 6 {
            expected_attempts.clear();
            assert_eq!(first.steps[0], state.steps[0]);
        }
        assert_eq!(
            final_step.attempts, expected_attempts,
            "case {case}: attempts changed"
        );
        let after_records = load_read(&db.begin_read().unwrap().tx).unwrap();
        for (key, node) in &before_records.nodes {
            assert_eq!(
                after_records.nodes.get(key),
                Some(node),
                "case {case}: existing node changed"
            );
        }
        if (3..=7).contains(&case) && case != 6 {
            let StepState::Active {
                active: ActiveStep::Executing { reply, slots },
            } = &state.steps[0].state
            else {
                unreachable!()
            };
            assert_eq!(invocations.len(), 3);
            for (index, (original, closed)) in slots.iter().zip(invocations).enumerate() {
                assert_eq!(closed.binding, original.binding);
                let expected = match &original.state {
                    SlotState::Terminal(t) => t.clone(),
                    SlotState::Started => InvocationTerminal {
                        result: StoredToolResult {
                            invocation_id: iid(2),
                            name: "observe.workspace".into(),
                            payload: ResultPayload::Indeterminate(IndeterminateCause::NoResult),
                        },
                        evidence: InvocationEvidence::Unavailable {
                            reason: EvidenceUnavailableReason::NoResult,
                        },
                    },
                    SlotState::Registered => InvocationTerminal {
                        result: StoredToolResult {
                            invocation_id: iid(index as u64 + 1),
                            name: "observe.workspace".into(),
                            payload: ResultPayload::NotExecuted,
                        },
                        evidence: InvocationEvidence::Unavailable {
                            reason: EvidenceUnavailableReason::NotDispatched,
                        },
                    },
                };
                assert_eq!(closed.terminal, expected, "case {case}, slot {index}");
            }
            assert_eq!(
                after_records.nodes[&(sid(1), NodeId(2))],
                Node::Step {
                    parent: NodeId(1),
                    turn_id: tid(1),
                    step_ordinal: 0,
                    content: StepContent {
                        assistant: reply.clone(),
                        results: invocations
                            .iter()
                            .map(|i| i.terminal.result.clone())
                            .collect()
                    }
                }
            );
        } else {
            assert!(invocations.is_empty());
            assert!(before_records.nodes == after_records.nodes);
        }
        let expected_usage = if case == 1 || case == 2 {
            UsageSummary::Partial {
                known: TokenUsage {
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    total_tokens: 0,
                },
                unknown_attempts: 1,
            }
        } else {
            UsageSummary::Complete {
                total: TokenUsage {
                    prompt_tokens: if case == 0 { 0 } else { 2 },
                    completion_tokens: if case == 0 { 0 } else { 3 },
                    total_tokens: if case == 0 { 0 } else { 5 },
                },
            }
        };
        assert!(
            matches!(&first.turn.state, TurnState::Closed {usage, ..} if *usage == expected_usage),
            "case {case}: usage"
        );
        assert!(matches!(
            db.auto_close(&lock, tid(1), now() + chrono::Duration::seconds(1))
                .unwrap(),
            AutoCloseResult::AlreadyClosed(_)
        ));
        assert_eq!(first, durable(&db, sid(1), tid(1)));
        assert!(
            after_records == load_read(&db.begin_read().unwrap().tx).unwrap(),
            "case {case}: repeated close changed records or audit log"
        );
        drop(lock);
        drop(db);
        assert!(SessionStore::open(&layout).is_ok());
    }
    let (_dir, _layout, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    db.accept_turn(&lock, accept(sid(1), tid(1))).unwrap();
    db.start_request(&lock, key(tid(1), 0, 0), now()).unwrap();
    db.stop_attempt(
        &lock,
        stopped(
            key(tid(1), 0, 0),
            StopCause::ProviderError { retryable: true },
        ),
        now(),
    )
    .unwrap();
    for cause in [
        StopCause::Cancelled,
        StopCause::ProviderError { retryable: false },
    ] {
        let mut state = durable(&db, sid(1), tid(1));
        state.steps[0].attempts[0].end = AttemptEnd::Stopped {
            cause,
            diagnostic: AttemptDiagnostic::default(),
        };
        assert!(matches!(
            plan_auto_close(&state, now()),
            Err(StoreError::Corruption { .. })
        ));
    }
    let mut state = durable(&db, sid(1), tid(1));
    state.turn.config.limits.max_model_attempts = 1;
    assert!(plan_auto_close(&state, now()).is_err());
}

// 同一有序规则服务 live 与恢复：槽位自身的停止优先，否则取关闭 Step 的外部停止。
#[test]
fn ordered_terminal_causes() {
    for (stop, expected) in [
        (DispatchStop::Cancelled, StopCause::Cancelled),
        (DispatchStop::Deadline, StopCause::Deadline),
        (DispatchStop::ProcessExit, StopCause::ProcessExit),
    ] {
        let (_dir, _layout, db) = store();
        let (lock, calls) = running(&db, sid(1), tid(1), 3, 1);
        finish(
            &db,
            &lock,
            &calls.bindings[0],
            Some(CepErrorCode::ExecutionFailed),
        );
        db.close_step(
            &lock,
            CloseStep::StopExecuting {
                key: step(tid(1), 0),
                stop,
            },
            now(),
        )
        .unwrap();
        let state = durable(&db, sid(1), tid(1));
        if let StepState::Closed {
            end: StepEnd::Stop { cause, .. },
            invocations,
        } = &state.steps[0].state
        {
            assert_eq!(cause, &expected);
            // 没轮到的槽位不带原因，原因只在 Step 的 end 里。
            assert_eq!(first_stopping_cause(invocations).unwrap(), None);
            assert!(
                invocations[1..]
                    .iter()
                    .all(|i| i.terminal.result.payload == ResultPayload::NotExecuted)
            );
        } else {
            panic!("closed stop")
        }
    }
    let (_dir, _layout, db) = store();
    let (lock, calls) = running(&db, sid(1), tid(1), 3, 1);
    finish(
        &db,
        &lock,
        &calls.bindings[0],
        Some(CepErrorCode::CallCancelled),
    );
    assert!(
        db.start_invocation(&lock, &calls.bindings[1], now())
            .is_err()
    );
    db.auto_close(&lock, tid(1), now()).unwrap();
    assert_eq!(
        summary(&db, sid(1), tid(1)).outcome,
        PublicTurnOutcome::Cancelled
    );
    let mut state = durable(&db, sid(1), tid(1));
    if let StepState::Closed { invocations, .. } = &mut state.steps[0].state {
        // NotExecuted 之后不能再出现有结果的调用。
        invocations[0].terminal.result.payload = ResultPayload::NotExecuted;
        invocations[0].terminal.evidence = InvocationEvidence::Unavailable {
            reason: EvidenceUnavailableReason::NotDispatched,
        };
        let binding = invocations[1].binding.clone();
        invocations[1].terminal = prepare_terminal(
            &binding,
            Ok(json!(null)),
            CellEvidenceTriple::refused_before_admission(&binding.identity.method),
        )
        .unwrap();
        assert!(first_stopping_cause(invocations).is_err());
    }
}

// 规范大小边界、真实证据与安全投影。
#[test]
fn payload_evidence_and_privacy() {
    let (_dir, _layout, db) = store();
    let (lock, calls) = running(&db, sid(1), tid(1), 1, 1);
    let binding = &calls.bindings[0];
    for (len, large) in [(65534, false), (65535, true)] {
        let value = json!("x".repeat(len));
        let term = prepare_terminal(
            binding,
            Ok(value),
            CellEvidenceTriple::refused_before_admission(&binding.identity.method),
        )
        .unwrap();
        match term.result.payload {
            ResultPayload::Succeeded(SuccessContent::Json { byte_length, .. }) => {
                assert_eq!((byte_length, large), (65536, false))
            }
            ResultPayload::Succeeded(SuccessContent::OutputTooLarge { byte_length, .. }) => {
                assert_eq!((byte_length, large), (65537, true))
            }
            _ => panic!("success"),
        }
    }
    let a = prepare_terminal(
        binding,
        Ok(json!({"z":"树","a":1})),
        CellEvidenceTriple::refused_before_admission(&binding.identity.method),
    )
    .unwrap();
    let b = prepare_terminal(
        binding,
        Ok(json!({"a":1,"z":"树"})),
        CellEvidenceTriple::refused_before_admission(&binding.identity.method),
    )
    .unwrap();
    assert_eq!(a, b);
    for case in 0..4 {
        let mut e = CellEvidenceTriple::refused_before_admission(&binding.identity.method);
        match case {
            0 => e.admission.version = 2,
            1 => e.effects.version = 2,
            2 => e.denied.version = 2,
            _ => e.admission.tool = "wrong".into(),
        };
        assert!(prepare_terminal(binding, Ok(json!(null)), e).is_err());
    }
    let mut ev = serde_json::to_value(CellEvidenceTriple::refused_before_admission(
        &binding.identity.method,
    ))
    .unwrap();
    ev["effects"]["future"] = json!(true);
    let ev = serde_json::from_value(ev).unwrap();
    assert!(prepare_terminal(binding, Ok(json!(null)), ev).is_ok());
    for (code, payload) in [
        (
            CepErrorCode::ExecutionFailed,
            ResultPayload::Failed(Box::new(CepError::new(
                CepErrorCode::ExecutionFailed,
                "",
                CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
                    operation: "internal".into(),
                    detail: None,
                }),
                Retriability::Transient,
            ))),
        ),
        (
            CepErrorCode::GuardDenied,
            ResultPayload::Failed(Box::new(CepError::new(
                CepErrorCode::ExecutionFailed,
                "",
                CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
                    operation: "internal".into(),
                    detail: None,
                }),
                Retriability::Transient,
            ))),
        ),
        (
            CepErrorCode::CallCancelled,
            ResultPayload::Indeterminate(IndeterminateCause::Cancelled),
        ),
        (
            CepErrorCode::OutcomeIndeterminate,
            ResultPayload::Indeterminate(IndeterminateCause::Reported),
        ),
    ] {
        let actual = terminal(binding, Some(code)).result.payload;
        match (&actual, &payload) {
            // 拒绝类错误归 Failed，错误码原样保留。
            (ResultPayload::Failed(e), ResultPayload::Failed(_)) => assert_eq!(e.code, code),
            _ => assert_eq!(actual, payload),
        }
    }
    assert_eq!(
        bound_provider_diagnostic(&"树".repeat(2049))
            .chars()
            .count(),
        2048
    );
    finish(&db, &lock, binding, None);
    db.auto_close(&lock, tid(1), now()).unwrap();
    let read = db.begin_read().unwrap();
    let bytes =
        serde_json::to_string(&read.query_audit(&filter(project(1)), None, 100).unwrap()).unwrap();
    let detail = serde_json::to_string(&read.turn_detail(sid(1), tid(1)).unwrap()).unwrap();
    for text in [bytes, detail] {
        for secret in [
            "private-input",
            "secret-result",
            "private-error",
            "input_schema",
            "profile_snapshot",
        ] {
            assert!(!text.contains(secret));
        }
    }
    assert!(!format!("{:?}", terminal(binding, None)).contains("secret-result"));
}

fn history(db: &SessionStore, anchor: &HistoryAnchor, budget: usize) -> Vec<Node> {
    let mut cursor = None;
    let mut fragments: BTreeMap<NodeId, String> = BTreeMap::new();
    loop {
        let page = db
            .begin_read()
            .unwrap()
            .history_page(anchor, cursor.as_ref(), budget)
            .unwrap();
        for f in page.fragments {
            let text = fragments.entry(f.node).or_default();
            assert_eq!(text.len() as u64, f.utf8_offset);
            text.push_str(&f.json_fragment);
        }
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    fragments
        .into_values()
        .map(|text| serde_json::from_str(&text).unwrap())
        .collect()
}

// 旧分支与固定分页同时保留，agent 仅从位置派生。
#[test]
fn tree_history_and_agents() {
    let (_dir, _layout, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    db.accept_turn(&lock, accept(sid(1), tid(1))).unwrap();
    assert!(db.rewind(&lock, NodeId(0), now()).is_err());
    db.start_request(&lock, key(tid(1), 0, 0), now()).unwrap();
    db.close_step(&lock, CloseStep::Finish(reply(key(tid(1), 0, 0), 0)), now())
        .unwrap();
    let old = db.begin_read().unwrap();
    let anchor = old.snapshot(sid(1), None).unwrap().history;
    let old_nodes = history(&db, &anchor, 7);
    assert_eq!(old_nodes.len(), 2);
    db.rewind(&lock, NodeId(0), now()).unwrap();
    assert_eq!(
        db.begin_read().unwrap().current_agent(sid(1)).unwrap(),
        None
    );
    assert!(
        db.begin_read()
            .unwrap()
            .snapshot(sid(1), None)
            .unwrap()
            .selected_turn
            .is_none()
    );
    let mut a = accept(sid(1), tid(2));
    a.session = AcceptSession::Existing { id: sid(1) };
    a.config.agent_name = "beta".into();
    a.agent_selection = AgentSelection::Inherited;
    assert!(db.accept_turn(&lock, a.clone()).is_err());
    a.agent_selection = AgentSelection::Explicit;
    db.accept_turn(&lock, a).unwrap();
    db.auto_close(&lock, tid(2), now()).unwrap();
    assert_eq!(
        db.begin_read().unwrap().session(sid(1)).unwrap().current,
        NodeId(3)
    );
    assert_eq!(old.session(sid(1)).unwrap().current, NodeId(2));
    assert_eq!(history(&db, &anchor, 11), old_nodes);
    let read = db.begin_read().unwrap();
    let log = read.log(sid(1)).unwrap();
    assert_eq!(log.nodes.len(), 4);
    assert_eq!(log.current_agent.as_deref(), Some("beta"));
    assert_eq!(read.list(None, 1).unwrap().items[0].node_count, 4);
    let selected = read.snapshot(sid(1), Some(tid(1))).unwrap();
    assert_eq!(selected.history.head, NodeId(3));
    assert_eq!(selected.selected_turn.unwrap().turn_id, tid(1));
    assert_eq!(history(&db, &selected.history, 1000).len(), 1);
    assert!(read.turn_detail(sid(2), tid(1)).is_err());
    assert!(read.history_page(&anchor, None, 0).is_err());
    for cursor in [
        HistoryCursor {
            anchor: anchor.clone(),
            node: NodeId(3),
            utf8_offset: 0,
        },
        HistoryCursor {
            anchor: anchor.clone(),
            node: NodeId(1),
            utf8_offset: u64::MAX,
        },
        HistoryCursor {
            anchor: HistoryAnchor {
                session_id: sid(2),
                head: NodeId(2),
            },
            node: NodeId(1),
            utf8_offset: 0,
        },
    ] {
        assert!(read.history_page(&anchor, Some(&cursor), 10).is_err());
    }
    let raw = String::from_utf8(
        canonical(
            &read
                .node(NodeRef {
                    session_id: sid(1),
                    node: NodeId(1),
                })
                .unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let offset = raw.find('用').unwrap() + 1;
    assert!(
        read.history_page(
            &anchor,
            Some(&HistoryCursor {
                anchor: anchor.clone(),
                node: NodeId(1),
                utf8_offset: offset as u64
            }),
            10
        )
        .is_err()
    );
    db.rewind(&lock, NodeId(2), now()).unwrap();
    assert_eq!(
        db.begin_read()
            .unwrap()
            .current_agent(sid(1))
            .unwrap()
            .as_deref(),
        Some("alpha")
    );
    assert!(db.rewind(&lock, NodeId(999), now()).is_err());
    assert!(matches!(
        db.accept_turn(&lock, accept(sid(1), tid(1))).unwrap(),
        AcceptedTurn::Existing(_)
    ));
}

// 全局高水位固定，过滤空页也前进，历史日志不投影成当前终态。
#[test]
fn audit_snapshots_and_pagination() {
    let (_dir, _layout, db) = store();
    let (lock, calls) = running(&db, sid(1), tid(1), 22, 1);
    let registered = db.begin_read().unwrap();
    for b in &calls.bindings {
        finish(&db, &lock, b, None);
    }
    db.auto_close(&lock, tid(1), now()).unwrap();
    let read = db.begin_read().unwrap();
    let query = registered
        .query_audit(&filter(project(1)), None, 100)
        .unwrap();
    assert_eq!(query.rows.len(), 22);
    assert!(
        query
            .rows
            .iter()
            .all(|r| matches!(r.state, OperatorInvocationState::Registered))
    );
    let tail = read
        .tail_audit(&filter(project(2)), ChangeSeq(0), 4)
        .unwrap();
    assert!(tail.rows.is_empty());
    assert!(tail.next_after > ChangeSeq(0));
    let first = read
        .tail_audit(&filter(project(1)), ChangeSeq(0), 2)
        .unwrap();
    assert_eq!(first.rows[0].sequence, ChangeSeq(1));
    assert!(matches!(
        first.rows[0].row.state,
        OperatorInvocationState::Registered
    ));
    let second = read
        .tail_audit(&filter(project(1)), first.next_after, 2)
        .unwrap();
    assert!(second.rows[0].sequence > first.next_after);
    assert!(
        read.tail_audit(&filter(project(1)), ChangeSeq(u64::MAX), 1)
            .is_err()
    );
    assert!(read.query_audit(&filter(project(1)), None, 0).is_err());
    let status = read.operator_status(&project(1)).unwrap();
    assert_eq!(status.recent.len(), 20);
    assert_eq!(status.recent[0].invocation_id, iid(22));
    let replay = read.replay_audit(&project(1), sid(1), tid(1)).unwrap();
    assert_eq!(replay.calls.len(), 22);
    assert_eq!(replay.calls[0].invocation_id, iid(1));
    assert!(read.replay_audit(&project(2), sid(1), tid(1)).is_err());
    let p1 = read.query_audit(&filter(project(1)), None, 7).unwrap();
    let p2 = read.query_audit(&filter(project(1)), p1.next, 100).unwrap();
    assert_eq!(p1.rows.len() + p2.rows.len(), 22);
}

// 独立损坏变体均严格拒绝，无自动修复。
#[test]
fn strict_open_rejects_corruption() {
    for case in 0..18 {
        let (_dir, layout, db) = store();
        let (lock, calls) = running(&db, sid(1), tid(1), 2, 1);
        finish(&db, &lock, &calls.bindings[0], None);
        db.auto_close(&lock, tid(1), now()).unwrap();
        drop(lock);
        drop(db);
        corrupt_state(&layout, |r| match case {
            0 => {
                r.nodes.remove(&(sid(1), NodeId(0)));
            }
            1 => {
                r.nodes.insert((sid(1), NodeId(3)), Node::Root);
            }
            2 => {
                if let Node::UserMessage { parent, .. } =
                    r.nodes.get_mut(&(sid(1), NodeId(1))).unwrap()
                {
                    *parent = NodeId(1);
                }
            }
            3 => r.sessions.get_mut(&sid(1)).unwrap().current = NodeId(999),
            4 => r.sessions.get_mut(&sid(1)).unwrap().active_turn = Some(tid(99)),
            5 => r.turns.get_mut(&tid(1)).unwrap().state = TurnState::Running,
            6 => {
                r.index.remove(&iid(1));
            }
            7 => {
                r.index.insert(iid(99), r.index[&iid(1)].clone());
            }
            8 => r.index.get_mut(&iid(1)).unwrap().last_change = ChangeSeq(0),
            9 => {
                r.changes.remove(&ChangeSeq(1));
            }
            10 => r.last_change = ChangeSeq(999),
            11 => r.index.get_mut(&iid(1)).unwrap().address.part_ordinal = 99,
            12 => r.steps.get_mut(&step(tid(1), 0)).unwrap().attempts[0].attempt = 2,
            13 => {
                if let TurnState::Closed { notice, .. } =
                    &mut r.turns.get_mut(&tid(1)).unwrap().state
                {
                    *notice = None;
                }
            }
            14 => {
                if let TurnState::Closed { usage, .. } =
                    &mut r.turns.get_mut(&tid(1)).unwrap().state
                {
                    *usage = UsageSummary::Partial {
                        known: TokenUsage {
                            prompt_tokens: 0,
                            completion_tokens: 0,
                            total_tokens: 0,
                        },
                        unknown_attempts: 99,
                    };
                }
            }
            15 => {
                if let Node::Step { content, .. } = r.nodes.get_mut(&(sid(1), NodeId(2))).unwrap() {
                    content.results.clear();
                }
            }
            16 => {
                r.changes.get_mut(&ChangeSeq(1)).unwrap().row.state =
                    OperatorInvocationState::Started
            }
            _ => r.turns.get_mut(&tid(1)).unwrap().config_digest = [0; 32],
        });
        assert!(
            matches!(
                SessionStore::open(&layout),
                Err(StoreError::Corruption { .. })
            ),
            "case {case}"
        );
    }
    for field in ["store_kind", "store_format_version", "last_change_seq"] {
        let (_d, layout, db) = store();
        drop(db);
        let raw = redb::Database::open(layout.lattice_root().join("lattice.redb")).unwrap();
        let tx = raw.begin_write().unwrap();
        {
            tx.open_table(META).unwrap().remove(field).unwrap();
        }
        tx.commit().unwrap();
        drop(raw);
        assert!(SessionStore::open(&layout).is_err());
    }
    let (_d, layout, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    db.accept_turn(&lock, accept(sid(1), tid(1))).unwrap();
    drop(lock);
    drop(db);
    corrupt_json(&layout, SESSIONS, &sid(1).to_string(), |v| {
        v["unknown"] = json!("secret");
    });
    assert!(SessionStore::open(&layout).is_err());
}

// 真实文件事务上的前/后提交故障分类，多个等待者一致停写。
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
                    || with_fault(point, || db.accept_turn(&lock, accept(sid(1), tid(1)))),
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
                                    db.accept_turn(&lock, accept(sid(n), tid(n))).map(|_| ())
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
                .turn_record(tid(1))
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

// 纯规划也要拒绝不一致的输入，不能借用 strict-open 的保证。
#[test]
fn planner_rejects_inconsistent_durable_facts() {
    let (_dir, _layout, db) = store();
    let (lock, _) = running(&db, sid(1), tid(1), 2, 1);
    let source = durable(&db, sid(1), tid(1));
    for case in 0..4 {
        let mut state = source.clone();
        match case {
            0 => state.session.current = NodeId(0),
            1 => state.next_node = state.turn.user_node,
            2 => {
                if let StepState::Active {
                    active: ActiveStep::Executing { reply, .. },
                } = &mut state.steps[0].state
                {
                    reply.provider_origin = Some(ProviderOrigin::Gemini);
                }
            }
            _ => state.turn.user_node = NodeId(0),
        }
        assert!(
            matches!(
                plan_auto_close(&state, now()),
                Err(StoreError::Corruption { .. })
            ),
            "case {case}"
        );
    }
    db.auto_close(&lock, tid(1), now()).unwrap();
    let mut closed = durable(&db, sid(1), tid(1));
    if let StepState::Closed { invocations, .. } = &mut closed.steps[0].state {
        invocations[0].binding.identity.turn_id = tid(99);
        invocations[1].binding.identity.turn_id = tid(99);
    }
    assert!(plan_auto_close(&closed, now()).is_err());
}

// 表类型/版本与三种旧库形态逐项拒绝。
#[test]
fn schema_and_legacy_rejection_matrix() {
    for old_kind in 0..3 {
        for location in 0..3 {
            let dir = tempfile::tempdir().unwrap();
            let layout = StateLayout::in_state_root(dir.path());
            fs::create_dir_all(layout.project_dir(&project(9))).unwrap();
            let path = if location == 0 {
                layout.lattice_root().join("sessions.git")
            } else {
                layout.project_store(
                    &project(if location == 1 { 1 } else { 9 }),
                    "invocations.git",
                )
            };
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            match old_kind {
                0 => fs::create_dir(&path).unwrap(),
                1 => fs::write(&path, b"old").unwrap(),
                _ => symlink("missing", &path).unwrap(),
            };
            for result in [SessionStore::open(&layout), SessionStore::create(&layout)] {
                assert!(
                    matches!(result, Err(StoreError::LegacyStores { paths }) if paths == vec![path.clone()])
                );
            }
            assert!(!layout.lattice_root().join("lattice.redb").exists());
        }
    }
    for case in 0..5 {
        let (_d, layout, db) = store();
        drop(db);
        let raw = redb::Database::open(layout.lattice_root().join("lattice.redb")).unwrap();
        let tx = raw.begin_write().unwrap();
        match case {
            0 => {
                tx.open_table(META)
                    .unwrap()
                    .insert(
                        "store_kind",
                        encode(&String::from("wrong")).unwrap().as_slice(),
                    )
                    .unwrap();
            }
            1 => {
                tx.open_table(META)
                    .unwrap()
                    .insert("store_format_version", encode(&2u32).unwrap().as_slice())
                    .unwrap();
            }
            2 => {
                tx.delete_table(SESSIONS).unwrap();
                tx.open_table(redb::TableDefinition::<u64, &[u8]>::new("sessions"))
                    .unwrap();
            }
            3 => {
                tx.delete_table(STEPS).unwrap();
            }
            _ => {
                tx.open_table(META)
                    .unwrap()
                    .insert("last_change_seq", &[0u8, 0, 0, 99, 48][..])
                    .unwrap();
            }
        }
        tx.commit().unwrap();
        drop(raw);
        assert!(SessionStore::open(&layout).is_err(), "case {case}");
    }
    let (_d, layout, db) = store();
    assert!(matches!(
        SessionStore::open(&layout),
        Err(StoreError::Database(
            redb::DatabaseError::DatabaseAlreadyOpen
        ))
    ));
    drop(db);
}

// 外部输入边界，不降低原断言。
#[test]
fn input_and_evidence_boundaries() {
    let mut c = config();
    let base = codec::canonical(&(c.catalog.runtime_build_id.clone(), &c.catalog.entries))
        .unwrap()
        .len();
    c.catalog.entries[0]
        .description
        .push_str(&"x".repeat(MAX_CATALOG_SNAPSHOT_BYTES - base));
    c.catalog =
        ToolCatalogSnapshot::new(c.catalog.runtime_build_id.clone(), c.catalog.entries).unwrap();
    assert!(validate::config(&c).is_ok());
    let mut entries = c.catalog.entries.clone();
    entries[0].description.push('x');
    assert!(ToolCatalogSnapshot::new(c.catalog.runtime_build_id, entries).is_err());
    let (_d, _l, db) = store();
    let (lock, calls) = running(&db, sid(1), tid(1), 2, 1);
    let mut receipt = terminal(&calls.bindings[0], None);
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
    let mut receipt = terminal(&calls.bindings[0], None);
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
            terminal: terminal(&calls.bindings[0], None),
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
    let e = json!({"seq":0,"cell":{"ctx_op":"alloc","ext_impact":"none","scope":"workspace"},"scope":{"scope":"workspace","trust":"in","channel":"workspace"},"target":{"kind":"path","path":"a"},"source":"operation"});
    let effect: lancet_protocol::evidence::ObservedEffect = serde_json::from_value(e).unwrap();
    let mut evidence = CellEvidenceTriple::refused_before_admission("observe.workspace");
    evidence.effects.effects.push(effect.clone());
    let value = prepare_terminal(
        &calls.bindings[0],
        Err(CepError::new(
            CepErrorCode::ExecutionFailed,
            "failure",
            CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
                operation: "internal".into(),
                detail: None,
            }),
            Retriability::Transient,
        )),
        evidence.clone(),
    )
    .unwrap();
    assert!(
        matches!(value.evidence(),InvocationEvidence::Recorded(e) if e.effects.effects.len()==1)
    );
    evidence.effects.effects.push(effect);
    assert!(prepare_terminal(&calls.bindings[0], Ok(json!({})), evidence).is_err());
    let mut ev = serde_json::to_value(CellEvidenceTriple::refused_before_admission(
        "observe.workspace",
    ))
    .unwrap();
    ev["admission"]["max_boundary"] = json!("unknown_trust");
    assert!(serde_json::from_value::<CellEvidenceTriple>(ev).is_err());
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
    let mut f = filter(project(1));
    f.outcome = Some(ToolResultOutcome::Succeeded);
    assert_eq!(
        db.begin_read()
            .unwrap()
            .query_audit(&f, None, 100)
            .unwrap()
            .rows
            .len(),
        1
    );
    f.outcome = Some(ToolResultOutcome::NotExecuted);
    f.method = Some("other".into());
    assert!(
        db.begin_read()
            .unwrap()
            .query_audit(&f, None, 100)
            .unwrap()
            .rows
            .is_empty()
    );
}

// 每一类领域事务都经过同一提交失败边界；重开不出现半组事实。
#[test]
fn transaction_failure_matrix() {
    for phase in 0..11 {
        for point in [FaultPoint::UnknownBefore, FaultPoint::UnknownAfter] {
            let (_dir, layout, db) = store();
            let lock = db.try_lock_session(sid(1)).unwrap();
            db.accept_turn(&lock, accept(sid(1), tid(1))).unwrap();
            if phase > 0 {
                db.start_request(&lock, key(tid(1), 0, 0), now()).unwrap();
            }
            let calls = if phase >= 4 && phase != 9 {
                Some(
                    db.register_calls(&lock, reply(key(tid(1), 0, 0), 2), &[iid(1), iid(2)], now())
                        .unwrap(),
                )
            } else {
                None
            };
            if phase >= 5 && phase != 9 {
                db.start_invocation(&lock, &calls.as_ref().unwrap().bindings[0], now())
                    .unwrap();
            }
            if phase >= 6 && phase != 9 {
                db.finish_invocation(
                    &lock,
                    TerminalInput {
                        invocation_id: iid(1),
                        terminal: terminal(&calls.as_ref().unwrap().bindings[0], None),
                    },
                    now(),
                )
                .unwrap();
            }
            if phase == 8 {
                db.auto_close(&lock, tid(1), now()).unwrap();
            }
            if phase == 10 {
                finish(&db, &lock, &calls.as_ref().unwrap().bindings[1], None);
            }
            let read = db.begin_read().unwrap();
            let before = codec::load_read(&read.tx).unwrap();
            let result = with_fault(point, || match phase {
                0 => db.start_request(&lock, key(tid(1), 0, 0), now()),
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
                        terminal: terminal(&calls.as_ref().unwrap().bindings[0], None),
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
                7 => db.auto_close(&lock, tid(1), now()).map(|_| ()),
                8 => db.rewind(&lock, NodeId(0), now()).map(|_| ()),
                9 => db
                    .register_calls(&lock, reply(key(tid(1), 0, 0), 2), &[iid(1), iid(2)], now())
                    .map(|_| ()),
                _ => db
                    .close_step(
                        &lock,
                        CloseStep::Continue {
                            key: step(tid(1), 0),
                        },
                        now(),
                    )
                    .map(|_| ()),
            });
            assert!(matches!(
                result,
                Err(StoreError::CommitFailed {
                    kind: CommitFailureKind::OutcomeUnknown,
                    ..
                })
            ));
            assert!(before == codec::load_read(&read.tx).unwrap());
            let after = codec::load_read(&db.begin_read().unwrap().tx).unwrap();
            assert_eq!(
                before == after,
                point == FaultPoint::UnknownBefore,
                "phase {phase}"
            );
            validate::records(&after).unwrap();
            drop(read);
            drop(lock);
            drop(db);
            let reopened = SessionStore::open(&layout).unwrap();
            assert!(after == codec::load_read(&reopened.begin_read().unwrap().tx).unwrap());
        }
    }
}
