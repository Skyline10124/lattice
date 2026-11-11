// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::codec::*;
use super::*;
use lattice_conversation::{
    CostEstimate, InvocationId, MessagePart, MessageRole, MessageSemantic, ModelAttemptKey,
    ModelId, NativeMessage, OpaqueProviderState, Protocol, ProviderId, SessionId, TokenUsage,
    ToolResultOutcome, TurnId, UnboundToolCall, UsageCompleteness,
};
use lattice_store::StateLayout;
use redb::ReadableTable;
use serde_json::{Value, json};
use std::{fs, os::unix::fs::symlink, sync::Arc};

fn now() -> Timestamp {
    "2026-09-26T00:00:00Z".parse().unwrap()
}

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

fn sid(n: u64) -> SessionId {
    SessionId::parse(&format!("{n:032x}")).unwrap()
}

fn tid(n: u64) -> TurnId {
    TurnId::parse(&format!("00000000-0000-4000-8000-{n:012x}")).unwrap()
}

fn iid(n: u64) -> InvocationId {
    InvocationId::parse(&format!("00000000-0000-4000-8000-{n:012x}")).unwrap()
}

fn project(n: u8) -> ProjectPath {
    ProjectPath::from_bytes(format!("/project-{n}")).expect("构造测试项目路径")
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

fn agent(name: &str) -> AgentName {
    AgentName::parse(name).unwrap()
}

fn provider(id: &str) -> ProviderId {
    ProviderId::parse(id).unwrap()
}

fn model(id: &str) -> ModelId {
    ModelId::parse(id).unwrap()
}

fn timeout(secs: u64) -> TurnTimeout {
    TurnTimeout::new(secs).unwrap()
}

fn config() -> FrozenTurnConfig {
    FrozenTurnConfig {
        agent_name: agent("alpha"),
        model: model("model"),
        provider: provider("openai"),
        effort: None,
        system_prompt: None,
        catalog: ToolCatalogSnapshot::new(vec![ToolCatalogEntry {
            name: "observe__workspace".into(),
            tool_identity: "lancet-node:observe@v1".into(),
            description: "observe".into(),
            input_schema: json!({"type":"object"}),
        }])
        .unwrap(),
        limits: TurnExecutionLimits {
            timeout_secs: TurnTimeout::default(),
            max_model_steps: 32,
            max_model_attempts: 3,
        },
    }
}

fn accept(s: SessionId, t: TurnId) -> AcceptTurn {
    let cfg = config();
    AcceptTurn {
        session_id: s,
        turn_id: t,
        project_path: project(1),
        input: "用户问题🌳".into(),
        settings_version: SessionSettings::for_test(&cfg).version().unwrap(),
        config: cfg,
        accepted_at: now(),
    }
}

// 通用夹具在首次写入前自动创建会话并对齐当前设置版本；设置本身的校验与拒绝测试直接调用领域入口。
fn accept_prepared(
    db: &SessionStore,
    lock: &SessionExecutionLock,
    mut input: AcceptTurn,
) -> Result<AcceptedTurn> {
    if db.begin_read()?.find_session(input.session_id)?.is_none()
        && db.begin_read()?.turn_node(input.turn_id)?.is_none()
        && lock.session_id() == input.session_id
    {
        db.create_session(
            lock,
            input.project_path.clone(),
            SessionSettings::for_test(&input.config),
            input.accepted_at,
        )?;
    }
    if let Ok(settings) = db.begin_read()?.session_settings(input.session_id) {
        input.settings_version = settings.version()?;
    }
    db.accept_turn(lock, input)
}

fn origin() -> AttemptOrigin {
    AttemptOrigin {
        protocol: AttemptProtocol::ChatCompletions,
        provider_id: provider("openai"),
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
                    name: "observe__workspace".into(),
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
        usage_kind: lattice_conversation::UsageCompleteness::Partial,
        cost: None,
        key: k,
        origin: origin(),
        usage: Some(TokenUsage {
            input: Some(2),
            output: Some(3),
            ..Default::default()
        }),
        message: NativeMessage::new(
            MessageRole::Assistant,
            Some(AttemptOrigin {
                protocol: Protocol::ChatCompletions,
                ..origin()
            }),
            parts,
        ),
    }
}

fn stopped(k: ModelAttemptKey, cause: StopCause) -> AttemptStopped {
    AttemptStopped {
        usage_kind: lattice_conversation::UsageCompleteness::Partial,
        cost: None,
        key: k,
        origin: origin(),
        usage: None,
        diagnostic: AttemptDiagnostic {
            provider_error: matches!(cause, StopCause::ProviderError { .. })
                .then(|| "old provider diagnostic".into()),
            ..Default::default()
        },
        retry_delay_ms: matches!(cause, StopCause::ProviderError { retryable: true })
            .then_some(250),
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
    accept_prepared(db, &lock, accept(s, t)).unwrap();
    db.start_request(&lock, key(t, 0, 0), origin(), now())
        .unwrap();
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

fn ok_reply() -> ToolReply {
    ToolReply::Succeeded(json!({"secret-result": 42}))
}

fn failed_reply(code: &str) -> ToolReply {
    ToolReply::Failed(ToolError {
        code: code.into(),
        message: "private-error".into(),
    })
}

fn terminal(binding: &InvocationBinding, reply: ToolReply) -> InvocationTerminal {
    prepare_terminal(binding, reply)
}

fn finish(
    db: &SessionStore,
    lock: &SessionExecutionLock,
    binding: &InvocationBinding,
    reply: ToolReply,
) {
    let started = db.start_invocation(lock, binding, now()).unwrap();
    assert_eq!(started.binding(), binding);
    db.finish_invocation(
        lock,
        TerminalInput {
            invocation_id: binding.invocation_id,
            terminal: terminal(binding, reply),
        },
        now(),
    )
    .unwrap();
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

#[test]
fn structured_result_wire() {
    let wire = json!({
        "invocation_id": "00000000-0000-4000-8000-000000000001",
        "name": "observe__workspace",
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

#[test]
fn domain_types_v9_wire() {
    let settings_wire = json!({
        "profile": {
            "agent_name": "coder",
            "description": "coding agent",
            "default_model": "openai/org/gpt-5",
            "default_effort": "medium",
            "timeout_secs": 300,
            "retry": {
                "max_retries": 3,
                "initial_delay_ms": 500,
                "max_delay_ms": 3000
            }
        },
        "system_prompt": "系统提示",
        "model": "openai/org/gpt-5",
        "effort": "medium"
    });
    let settings: SessionSettings = serde_json::from_value(settings_wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(&settings).unwrap(), settings_wire);
    let encoded_settings = encode(&settings).unwrap();
    assert_eq!(&encoded_settings[..4], &9_u32.to_be_bytes());
    assert_eq!(
        decode_session_settings("s1", &encoded_settings).unwrap(),
        settings
    );

    let user_wire = json!({
        "kind": "user_message",
        "parent": 0,
        "turn_id": "00000000-0000-4000-8000-000000000001",
        "accepted_at": "2026-09-27T00:00:00Z",
        "agent_name": "coder",
        "model": "org/gpt-5",
        "provider": "openai",
        "effort": "medium",
        "limits": {
            "timeout_secs": 300,
            "max_model_steps": 32,
            "max_model_attempts": 4
        },
        "config_digest": "cd".repeat(32),
        "message": {
            "role": "user",
            "provider_origin": null,
            "parts": [
                {
                    "semantic": { "text": { "text": "hello" } },
                    "replay_state": null
                }
            ]
        }
    });
    let user: Node = serde_json::from_value(user_wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(&user).unwrap(), user_wire);
    let encoded_user = encode(&user).unwrap();
    assert_eq!(&encoded_user[..4], &9_u32.to_be_bytes());
    assert!(decode::<Node>("nodes", "n1", &encoded_user).unwrap() == user);
}

#[test]
fn creation_and_legacy() {
    let dir = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(dir.path());
    assert!(matches!(
        SessionStore::open(&layout),
        Err(StoreError::NotInitialized { .. })
    ));
    assert!(!layout.lattice_root().exists());
    let legacy_proj = layout.projects_dir().join("p9");
    fs::create_dir_all(&legacy_proj).unwrap();
    let old = layout.lattice_root().join("sessions.git");
    fs::write(&old, b"old").unwrap();
    let inv = legacy_proj.join("invocations.git");
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
        accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
        drop(lock);
        drop(db);
        let db = SessionStore::create(&layout).unwrap();
        assert!(
            db.begin_read()
                .unwrap()
                .turn_node(tid(1))
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
fn schema_and_legacy_rejection_matrix() {
    for old_kind in 0..3 {
        for location in 0..3 {
            let dir = tempfile::tempdir().unwrap();
            let layout = StateLayout::in_state_root(dir.path());
            fs::create_dir_all(layout.projects_dir().join("p9")).unwrap();
            let path = if location == 0 {
                layout.lattice_root().join("sessions.git")
            } else {
                layout
                    .projects_dir()
                    .join(if location == 1 { "p1" } else { "p9" })
                    .join("invocations.git")
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
    for case in 0..10 {
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
                    .insert(
                        "store_format_version",
                        encode(&(VERSION + 1)).unwrap().as_slice(),
                    )
                    .unwrap();
            }
            2 => {
                tx.delete_table(SESSIONS).unwrap();
                tx.open_table(redb::TableDefinition::<u64, &[u8]>::new("sessions"))
                    .unwrap();
            }
            3 => {
                tx.delete_table(CONFIGS).unwrap();
            }
            4 | 5 | 7 | 8 | 9 => {
                let mut meta = tx.open_table(META).unwrap();
                // 固定旧版元信息，独立于当前编码器核对严格打开。
                let old_version = match case {
                    4 => 3_u32,
                    5 => 4_u32,
                    7 => 6_u32,
                    8 => 7_u32,
                    _ => 8_u32,
                };
                for (key, value) in [
                    ("store_kind", "\"lattice-session\"".to_owned()),
                    ("store_format_version", old_version.to_string()),
                ] {
                    let mut bytes = old_version.to_be_bytes().to_vec();
                    bytes.extend(value.as_bytes());
                    meta.insert(key, bytes.as_slice()).unwrap();
                }
            }
            _ => {
                tx.open_table(META)
                    .unwrap()
                    .insert("store_kind", &[0u8, 0, 0, 99, b'"', b'x', b'"'][..])
                    .unwrap();
            }
        }
        tx.commit().unwrap();
        drop(raw);
        let error = SessionStore::open(&layout)
            .err()
            .unwrap_or_else(|| panic!("case {case}"));
        if matches!(case, 4 | 5 | 7 | 8) {
            let expected = match case {
                4 => 3,
                5 => 4,
                7 => 6,
                _ => 7,
            };
            let path = layout.lattice_root().join("lattice.redb");
            assert!(error.to_string().contains(&path.display().to_string()));
            assert!(matches!(
                error,
                StoreError::UnsupportedVersion { path: found_path, found, supported: VERSION, .. }
                    if found_path == path && found == expected
            ));
        }
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

fn node_at(db: &SessionStore, n: u64) -> Node {
    db.begin_read()
        .unwrap()
        .node(NodeRef {
            session_id: sid(1),
            node: NodeId(n),
        })
        .unwrap()
}
fn detail(db: &SessionStore) -> TurnDetail {
    db.begin_read()
        .unwrap()
        .turn_detail(sid(1), tid(1))
        .unwrap()
}
fn records(db: &SessionStore) -> Records {
    load_read(&db.begin_read().unwrap().tx).unwrap()
}

#[test]
fn keys_versions_and_encoding() {
    for n in [0, 1, 255, 256, u64::MAX] {
        assert_eq!(
            decode_node_key(&node_key(sid(1), NodeId(n))).unwrap(),
            (sid(1), NodeId(n))
        );
    }
    assert!(node_key(sid(1), NodeId(255)) < node_key(sid(1), NodeId(256)));
    assert!(node_key(sid(1), NodeId(u64::MAX)) < node_key(sid(2), NodeId(0)));
    for bad in [
        vec![],
        vec![b'a'; 39],
        vec![b'a'; 41],
        vec![b'A'; 40],
        vec![255; 40],
    ] {
        assert!(decode_node_key(&bad).is_err());
    }
    for digest in [[0; 32], [255; 32], [171; 32]] {
        assert_eq!(decode_config_key(&config_key(&digest)).unwrap(), digest);
    }
    for key in [
        "a".repeat(63),
        "a".repeat(65),
        "A".repeat(64),
        "g".repeat(64),
    ] {
        assert!(decode_config_key(&key).is_err());
    }
    assert_eq!(
        decode::<Node>("nodes", "0", &encode(&Node::Root).unwrap()).unwrap(),
        Node::Root
    );
    for version in (0..VERSION).chain([VERSION + 1, u32::MAX]) {
        let mut bytes = version.to_be_bytes().to_vec();
        bytes.extend(b"{\"kind\":\"root\"}");
        let error = attach_path(
            std::path::Path::new("/test/lattice.redb"),
            decode::<Node>("nodes", "0", &bytes),
        )
        .unwrap_err();
        assert!(
            matches!(error, StoreError::UnsupportedVersion { path, found, supported: VERSION, .. } if path == std::path::Path::new("/test/lattice.redb") && found == version)
        );
    }
    for body in [
        b"".as_slice(),
        b"\xff",
        b"null",
        b"{}",
        b"{\"kind\":\"private-variant\"}",
        b"{\"kind\":\"root\",\"private-field\":true}",
        b"{\"kind\":\"root\"} trailing",
    ] {
        let mut bytes = VERSION.to_be_bytes().to_vec();
        bytes.extend(body);
        let error = decode::<Node>("nodes", "0", &bytes).unwrap_err();
        assert!(!format!("{error:?}").contains("private-"));
    }
    for bytes in [vec![], vec![0], vec![0; 3]] {
        assert!(decode::<Node>("nodes", "0", &bytes).is_err());
    }
    let (_d, _l, db) = store();
    {
        let read = db.begin_read().unwrap();
        let meta = read.tx.open_table(META).unwrap();
        assert_eq!(
            meta.get("store_format_version").unwrap().unwrap().value(),
            b"\x00\x00\x00\x099"
        );
    }
    let (_lock, _calls) = running(&db, sid(1), tid(1), 1, 1);
    let r = records(&db);
    assert_eq!(
        decode::<SessionRecord>("sessions", "1", &encode(&r.sessions[&sid(1)]).unwrap()).unwrap(),
        r.sessions[&sid(1)]
    );
    assert_eq!(
        decode::<Node>(
            "nodes",
            "2",
            &encode(&r.nodes[&(sid(1), NodeId(2))]).unwrap()
        )
        .unwrap(),
        r.nodes[&(sid(1), NodeId(2))]
    );
    assert_eq!(
        decode::<StoredTurnConfig>("configs", "1", &encode(&config().stored()).unwrap()).unwrap(),
        config().stored()
    );
    assert_eq!(
        decode::<NodeRef>("turn_index", "1", &encode(&r.turn_index[&tid(1)]).unwrap()).unwrap(),
        r.turn_index[&tid(1)]
    );
    assert_eq!(
        decode::<InvocationIndexEntry>(
            "invocation_index",
            "1",
            &encode(&r.index[&iid(1)]).unwrap()
        )
        .unwrap(),
        r.index[&iid(1)]
    );
    for (usage_kind, cost) in [
        (UsageCompleteness::Partial, None),
        (
            UsageCompleteness::Complete,
            Some(CostEstimate {
                usd: 0.0,
                exact: true,
            }),
        ),
        (
            UsageCompleteness::Partial,
            Some(CostEstimate {
                usd: 0.125,
                exact: false,
            }),
        ),
    ] {
        let mut node = r.nodes[&(sid(1), NodeId(2))].clone();
        let Node::Step { attempts, .. } = &mut node else {
            unreachable!()
        };
        attempts[0].usage = Some(TokenUsage {
            input: Some(10),
            output: Some(2),
            ..Default::default()
        });
        attempts[0].usage_kind = usage_kind;
        attempts[0].cost = cost;
        let bytes = encode(&node).unwrap();
        assert_eq!(decode::<Node>("nodes", "2", &bytes).unwrap(), node);
    }
    for invalid_cost in [-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut node = r.nodes[&(sid(1), NodeId(2))].clone();
        let Node::Step { attempts, .. } = &mut node else {
            unreachable!()
        };
        attempts[0].cost = Some(CostEstimate {
            usd: invalid_cost,
            exact: true,
        });
        assert!(encode(&node).is_err());
    }
    let mut damaged = serde_json::to_value(&r.nodes[&(sid(1), NodeId(2))]).unwrap();
    damaged["attempts"][0]["cost"] = json!({"usd":-1.0,"exact":true});
    let mut bytes = VERSION.to_be_bytes().to_vec();
    bytes.extend(serde_json::to_vec(&damaged).unwrap());
    assert!(matches!(
        decode::<Node>("nodes", "2", &bytes),
        Err(StoreError::Corruption { .. })
    ));
    for invalid in ["-1", "1.5", r#""many""#, "18446744073709551616"] {
        let mut damaged = serde_json::to_value(&r.nodes[&(sid(1), NodeId(2))]).unwrap();
        damaged["attempts"][0]["usage"]["input"] = json!(0);
        // 直接写入原始数字，保留超范围整数的十进制编码。
        let raw = serde_json::to_string(&damaged)
            .unwrap()
            .replace("\"input\":0", &format!("\"input\":{invalid}"));
        let mut bytes = VERSION.to_be_bytes().to_vec();
        bytes.extend(raw.as_bytes());
        assert!(
            matches!(
                decode::<Node>("nodes", "2", &bytes),
                Err(StoreError::Corruption { .. })
            ),
            "计数字段损坏未被拒绝：{invalid}"
        );
    }
}

#[test]
fn nested_unknown_fields_are_rejected() {
    let (_d, _l, db) = store();
    let (lock, calls) = running(&db, sid(1), tid(1), 1, 1);
    finish(
        &db,
        &lock,
        &calls.bindings[0],
        failed_reply("execution_failed"),
    );
    let original = serde_json::to_value(node_at(&db, 2)).unwrap();
    for pointer in [
        "/attempts/0/origin",
        "/attempts/0/usage",
        "/attempts/0/progress",
        "/attempts/0/progress/end",
        "/reply",
        "/reply/parts/1",
        "/reply/parts/1/semantic/tool_call/call",
        "/slots/0",
        "/slots/0/binding",
        "/slots/0/binding/identity",
        "/slots/0/progress",
        "/slots/0/progress/result/payload/payload",
    ] {
        let mut v = original.clone();
        v.pointer_mut(pointer)
            .unwrap_or_else(|| panic!("fixture {pointer}"))
            .as_object_mut()
            .unwrap()
            .insert("private-field".into(), json!(true));
        assert!(
            matches!(
                decode::<Node>("nodes", "2", &encode(&v).unwrap()),
                Err(StoreError::Corruption { .. })
            ),
            "{pointer}"
        );
    }
    for pointer in [
        "/attempts/0/origin",
        "/attempts/0/origin/protocol",
        "/attempts/0/origin/provider_id",
    ] {
        let mut value = original.clone();
        *value.pointer_mut(pointer).unwrap() = serde_json::Value::Null;
        assert!(
            decode::<Node>("nodes", "2", &encode(&value).unwrap()).is_err(),
            "{pointer}"
        );
    }
    let mut value = original;
    value["attempts"][0]["origin"]["protocol"] = json!("unknown");
    assert!(decode::<Node>("nodes", "2", &encode(&value).unwrap()).is_err());
    for pointer in ["", "/catalog", "/catalog/entries/0"] {
        let mut v = serde_json::to_value(config().stored()).unwrap();
        v.pointer_mut(pointer).unwrap()["private-field"] = json!(true);
        assert!(decode::<StoredTurnConfig>("configs", "key", &encode(&v).unwrap()).is_err());
    }
}

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
fn acceptance_deduplicates_configs_and_preserves_abandoned_nodes() {
    let (_d, layout, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
    let old = node_at(&db, 2);
    let mut beta_settings = SessionSettings::for_test(&config());
    beta_settings.profile.agent_name = agent("beta");
    db.update_settings(&lock, &project(1), beta_settings, now())
        .unwrap();
    let mut next = accept(sid(1), tid(2));
    next.config.agent_name = agent("beta");
    accept_prepared(&db, &lock, next).unwrap();
    assert_eq!(records(&db).configs.len(), 1);
    assert_eq!(node_at(&db, 2), old);
    assert_eq!(
        db.begin_read().unwrap().current_agent(sid(1)).unwrap(),
        Some(agent("beta"))
    );
    assert!(matches!(
        db.start_request(&lock, key(tid(1), 0, 0), origin(), now()),
        Err(StoreError::InvalidTransition { .. })
    ));
    let mut retry = accept(sid(1), tid(1));
    retry.config.agent_name = agent("ignored");
    assert!(matches!(
        accept_prepared(&db, &lock, retry).unwrap(),
        AcceptedTurn::Existing(t) if t.session_id == sid(1)
    ));
    let lock2 = db.try_lock_session(sid(2)).unwrap();
    for case in 0..3 {
        let mut conflicting = accept(sid(1), tid(1));
        match case {
            0 => conflicting.input.push('!'),
            1 => conflicting.project_path = project(2),
            _ => conflicting.session_id = sid(2),
        }
        assert!(
            matches!(
                accept_prepared(&db, if case == 2 { &lock2 } else { &lock }, conflicting),
                Err(StoreError::Conflict { .. })
            ),
            "重放已有 TurnId 时输入、项目路径或会话 ID 不一致必须返回冲突（case={case}）"
        );
    }
    let mut wrong_project = accept(sid(1), tid(99));
    wrong_project.project_path = project(2);
    assert!(
        matches!(
            accept_prepared(&db, &lock, wrong_project),
            Err(StoreError::BindingMismatch { .. })
        ),
        "向已有会话写入新 Turn 时项目路径不一致必须被拒绝"
    );
    let current = db.begin_read().unwrap().session(sid(1)).unwrap().current;
    assert!(
        matches!(
            db.rewind(&lock, NodeId(0), &project(2), now()),
            Err(StoreError::BindingMismatch { .. })
        ),
        "rewind 的项目路径不一致必须在会话库写入时被拒绝"
    );
    assert_eq!(
        db.begin_read().unwrap().session(sid(1)).unwrap().current,
        current,
        "被拒绝的 rewind 不能移动当前位置"
    );
    db.rewind(&lock, NodeId(0), &project(1), now()).unwrap();
    let mut next = accept(sid(1), tid(3));
    next.config.system_prompt = Some("different".into());
    accept_prepared(&db, &lock, next).unwrap();
    assert_eq!(records(&db).configs.len(), 2);
    assert_eq!(node_at(&db, 2), old);
    assert_eq!(node_at(&db, 5).parent(), Some(NodeId(0)));
    assert_eq!(
        db.begin_read().unwrap().list(None, 10).unwrap().items[0].unclosed_turn,
        Some(tid(3))
    );
    assert_eq!(
        db.begin_read().unwrap().log(sid(1)).unwrap().unclosed_turn,
        Some(tid(3))
    );
    drop(lock2);
    drop(lock);
    drop(db);
    let db = SessionStore::open(&layout).unwrap();
    assert_eq!(node_at(&db, 2), old);
}

#[test]
fn timeout_bounds_preserve_frozen_limits_and_session_settings_on_reopen() {
    for timeout_secs in [1, 300, 3600, 3601, MAX_TURN_TIMEOUT_SECS] {
        let (_dir, layout, db) = store();
        let lock = db.try_lock_session(sid(1)).unwrap();
        let mut first = accept(sid(1), tid(1));
        first.config.limits.timeout_secs = timeout(timeout_secs);
        let expected_first = first.config.clone();
        let expected_first_settings = SessionSettings::for_test(&first.config);
        first.settings_version = expected_first_settings.version().unwrap();
        db.create_session(&lock, project(1), expected_first_settings.clone(), now())
            .unwrap();
        db.accept_turn(&lock, first)
            .unwrap_or_else(|error| panic!("timeout_secs={timeout_secs}: {error}"));
        let read = db.begin_read().unwrap();
        assert_eq!(read.turn_config(sid(1), tid(1)).unwrap(), expected_first);
        assert_eq!(
            read.session_settings(sid(1)).unwrap(),
            expected_first_settings
        );
        drop(read);
        drop(lock);
        drop(db);

        let reopened = SessionStore::open(&layout)
            .unwrap_or_else(|error| panic!("timeout_secs={timeout_secs}: {error}"));
        let lock = reopened.try_lock_session(sid(1)).unwrap();
        let mut second = accept(sid(1), tid(2));
        second.config.agent_name = agent("beta");
        second.config.limits.timeout_secs = TurnTimeout::default();
        let expected_second = second.config.clone();
        let expected_second_settings = SessionSettings::for_test(&second.config);
        reopened
            .update_settings(
                &lock,
                &project(1),
                expected_second_settings.clone(),
                now() + chrono::Duration::seconds(1),
            )
            .unwrap();
        second.settings_version = expected_second_settings.version().unwrap();
        reopened.accept_turn(&lock, second).unwrap();
        drop(lock);
        drop(reopened);

        let reopened = SessionStore::open(&layout)
            .unwrap_or_else(|error| panic!("timeout_secs={timeout_secs}: {error}"));
        let read = reopened.begin_read().unwrap();
        assert_eq!(read.turn_config(sid(1), tid(1)).unwrap(), expected_first);
        assert_eq!(read.turn_config(sid(1), tid(2)).unwrap(), expected_second);
        assert_eq!(
            read.session_settings(sid(1)).unwrap(),
            expected_second_settings
        );
    }

    let (_dir, layout, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
    let mut second = accept(sid(1), tid(2));
    second.config.agent_name = agent("beta");
    db.update_settings(
        &lock,
        &project(1),
        SessionSettings::for_test(&second.config),
        now() + chrono::Duration::seconds(1),
    )
    .unwrap();
    accept_prepared(&db, &lock, second).unwrap();
    let base = records(&db);
    drop(lock);
    drop(db);
    for invalid_timeout in [0, MAX_TURN_TIMEOUT_SECS + 1] {
        let raw = redb::Database::open(layout.lattice_root().join("lattice.redb")).unwrap();
        let tx = raw.begin_write().unwrap();
        overwrite_records(&tx, &base);
        {
            let mut nodes = tx.open_table(NODES).unwrap();
            let mut wire = serde_json::to_value(&base.nodes[&(sid(1), NodeId(1))]).unwrap();
            wire["limits"]["timeout_secs"] = json!(invalid_timeout);
            nodes
                .insert(
                    node_key(sid(1), NodeId(1)).as_slice(),
                    encode(&wire).unwrap().as_slice(),
                )
                .unwrap();
        }
        tx.commit().unwrap();
        drop(raw);
        assert!(
            matches!(
                SessionStore::open(&layout),
                Err(StoreError::Corruption { .. })
            ),
            "invalid_timeout={invalid_timeout}"
        );
    }
}

#[test]
fn invalid_acceptance_and_lock_capabilities_leave_no_facts() {
    let (_d, _l, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    assert!(
        matches!(
            db.accept_turn(&lock, accept(sid(1), tid(1))),
            Err(StoreError::NotFound {
                kind: "session",
                ..
            })
        ),
        "不存在的会话调用 accept_turn 必须返回 NotFound"
    );
    assert!(records(&db).nodes.is_empty());

    let mut bad_settings = SessionSettings::for_test(&config());
    bad_settings.system_prompt = Some("x".repeat(2 * 1024 * 1024 + 1));
    assert!(
        matches!(
            db.create_session(&lock, project(1), bad_settings, now()),
            Err(StoreError::InvalidInput { .. })
        ),
        "设置无效时 create_session 必须拒绝且不落库"
    );
    assert!(records(&db).nodes.is_empty());

    let base_settings = SessionSettings::for_test(&config());
    let base_version = base_settings.version().unwrap();
    db.create_session(&lock, project(1), base_settings.clone(), now())
        .unwrap();
    assert!(matches!(
        db.create_session(&lock, project(1), base_settings, now()),
        Err(StoreError::SessionCollision { session_id }) if session_id == sid(1)
    ));
    let before = records(&db);

    for case in 0..8 {
        let mut a = accept(sid(1), tid(1));
        a.settings_version = base_version;
        let expected_detail = match case {
            0 => {
                a.input = "x".repeat(MAX_USER_INPUT_BYTES + 1);
                None
            }
            1 => {
                a.config.system_prompt = Some("x".repeat(2 * 1024 * 1024 + 1));
                None
            }
            2 => {
                a.config
                    .catalog
                    .entries
                    .push(a.config.catalog.entries[0].clone());
                None
            }
            3 => {
                a.config.limits.max_model_steps = 33;
                Some("invalid frozen configuration")
            }
            4 => {
                a.config.limits.max_model_attempts = 0;
                Some("invalid frozen configuration")
            }
            5 => {
                a.config.catalog.digest = [0; 32];
                None
            }
            6 => {
                a.config.limits.max_model_attempts = 12;
                Some("invalid frozen configuration")
            }
            7 => {
                use sha2::{Digest, Sha256};
                let first = a.config.catalog.entries[0].clone();
                let mut z = first.clone();
                z.name = "z_tool".into();
                a.config.catalog.entries = vec![z, first];
                let canonical = codec::canonical(&a.config.catalog.entries).unwrap();
                let mut hasher = Sha256::new();
                hasher.update(b"lattice-tool-catalog-snapshot-v1\0");
                hasher.update((canonical.len() as u64).to_be_bytes());
                hasher.update(&canonical);
                a.config.catalog.digest = hasher.finalize().into();
                None
            }
            _ => unreachable!(),
        };
        let result = db.accept_turn(&lock, a);
        if let Some(expected_detail) = expected_detail {
            assert!(
                matches!(&result, Err(StoreError::InvalidInput { field: "config", detail }) if detail == expected_detail),
                "{case}: {result:?}"
            );
        } else {
            assert!(result.is_err(), "{case}");
        }
        assert!(before == records(&db), "{case} 修改了持久记录");
    }
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
        db2.create_session(
            &lock,
            project(1),
            SessionSettings::for_test(&config()),
            now()
        ),
        Err(StoreError::InvalidLockCapability)
    ));
    assert!(matches!(
        db2.update_settings(
            &lock,
            &project(1),
            SessionSettings::for_test(&config()),
            now()
        ),
        Err(StoreError::InvalidLockCapability)
    ));
    assert!(matches!(
        db2.accept_turn(&lock, accept(sid(1), tid(1))),
        Err(StoreError::InvalidLockCapability)
    ));
    let mut a = accept(sid(1), tid(1));
    a.input = "x".repeat(MAX_USER_INPUT_BYTES);
    a.settings_version = base_version;
    db.accept_turn(&lock, a).unwrap();
    assert_eq!(
        db.begin_read().unwrap().session(sid(1)).unwrap().current,
        NodeId(2)
    );
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
    assert_eq!(&session[..4], &9u32.to_be_bytes(), "会话记录格式版本为 9");
    assert_no_endpoint("会话记录", &String::from_utf8_lossy(&session[4..]));
    let mut origins = 0;
    for ((_, id), node) in &r.nodes {
        let bytes = encode(node).unwrap();
        assert_eq!(&bytes[..4], &9u32.to_be_bytes(), "节点 {id:?} 格式版本为 9");
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

fn entry_nodes(entries: &[ConversationEntry]) -> Vec<u64> {
    entries.iter().map(|entry| entry.node().0).collect()
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

fn entry_ordinals(entries: &[ConversationEntry]) -> Vec<u64> {
    entries
        .iter()
        .filter_map(|entry| match entry {
            ConversationEntry::UserMessage { ordinal, .. } => Some(*ordinal),
            ConversationEntry::Step { .. } => None,
        })
        .collect()
}

#[test]
fn conversation_view_recent_turns_and_branch_ordinals() {
    let (_d, _l, db, lock) = conversation_tree();
    let read = db.begin_read().unwrap();
    let view = read.view(sid(1), 2).unwrap();
    assert_eq!(entry_nodes(&view.entries), vec![4, 5, 6, 7]);
    assert_eq!(entry_ordinals(&view.entries), vec![2, 3]);
    assert_eq!(view.session.current, NodeId(7));
    assert_eq!(view.current_agent.as_ref(), Some(&agent("beta")));
    assert!(
        matches!(&view.entries[3], ConversationEntry::Step { step, summary: Some(summary), .. }
        if step.progress == StepProgress::Reserved && summary.progress == StepProgress::Reserved)
    );
    assert_eq!(
        entry_nodes(&read.view(sid(1), u32::MAX).unwrap().entries),
        vec![1, 2, 3, 4, 5, 6, 7]
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
    assert_eq!(entry_ordinals(&view.entries), vec![1, 2]);
    assert_eq!(
        entry_ordinals(&[read
            .entry(NodeRef {
                session_id: sid(1),
                node: NodeId(8)
            })
            .unwrap()]),
        vec![2]
    );
    drop(read);
    db.rewind(&lock, NodeId(1), &project(1), now()).unwrap();
    let view = db.begin_read().unwrap().view(sid(1), 1).unwrap();
    assert_eq!(entry_nodes(&view.entries), vec![1]);
    assert_eq!(view.current_agent, Some(agent("beta")));
    db.rewind(&lock, NodeId(0), &project(1), now()).unwrap();
    let view = db.begin_read().unwrap().view(sid(1), 1).unwrap();
    assert_eq!(view.session.current, NodeId(0));
    assert!(view.entries.is_empty());
    assert_eq!(view.current_agent, Some(agent("beta")));
}

#[test]
fn conversation_history_pages_whole_turns_on_fixed_parent_chain() {
    let (_d, _l, db, lock) = conversation_tree();
    let read = db.begin_read().unwrap();
    let recent = read.history(sid(1), NodeId(6), 1).unwrap();
    assert_eq!(entry_nodes(&recent), vec![4, 5]);
    assert_eq!(entry_ordinals(&recent), vec![2]);
    let older = read.history(sid(1), NodeId(4), 1).unwrap();
    assert_eq!(entry_nodes(&older), vec![1, 2, 3]);
    assert_eq!(entry_ordinals(&older), vec![1]);
    let both = read.history(sid(1), NodeId(6), 2).unwrap();
    assert_eq!(both, [older.clone(), recent].concat());
    assert_eq!(read.history(sid(1), NodeId(6), u32::MAX).unwrap(), both);
    assert!(read.history(sid(1), NodeId(1), 1).unwrap().is_empty());
    drop(read);
    db.rewind(&lock, NodeId(2), &project(1), now()).unwrap();
    let branch = accept(sid(1), tid(4));
    accept_prepared(&db, &lock, branch).unwrap();
    let read = db.begin_read().unwrap();
    assert_eq!(read.history(sid(1), NodeId(6), 2).unwrap(), both);
    assert_eq!(
        entry_nodes(&read.history(sid(1), NodeId(8), 1).unwrap()),
        vec![1, 2]
    );
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
    assert_eq!(
        serde_json::to_value(&entry).unwrap(),
        json!({
            "kind": "step", "node": 2, "content": node_at(&db, 2), "step": expected.steps[0],
            "retries": [], "summary": expected.summary,
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
    let user = read
        .entry(NodeRef {
            node: NodeId(1),
            ..node
        })
        .unwrap();
    assert_eq!(
        serde_json::to_value(&user).unwrap(),
        json!({ "kind": "user_message", "node": 1, "content": node_at(&db, 1), "ordinal": 1 })
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
    let entries = read.history(sid(1), NodeId(3), 1).unwrap();
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
    assert_eq!(read.history(sid(1), NodeId(3), 1).unwrap(), entries);
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
    for damaged in [NodeId(2), NodeId(4)] {
        let (_d, _l, db, _lock) = conversation_tree();
        let node = NodeRef {
            session_id: sid(1),
            node: NodeId(7),
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
                node: NodeId(3),
                ..node
            })
            .is_ok()
        );
    }
}

#[test]
fn conversation_windows_skip_steps_outside_the_requested_turns() {
    let (_d, _l, db, _lock) = conversation_tree();
    let read = db.begin_read().unwrap();
    let view = read.view(sid(1), 1).unwrap();
    let history = read.history(sid(1), NodeId(6), 1).unwrap();
    drop(read);
    let tx = db.db.begin_write().unwrap();
    {
        let mut nodes = tx.open_table(NODES).unwrap();
        nodes
            .insert(
                node_key(sid(1), NodeId(2)).as_slice(),
                b"invalid".as_slice(),
            )
            .unwrap();
    }
    tx.commit().unwrap();
    let read = db.begin_read().unwrap();
    assert_eq!(read.view(sid(1), 1).unwrap(), view);
    assert_eq!(read.history(sid(1), NodeId(6), 1).unwrap(), history);
    assert_eq!(
        entry_ordinals(&[read
            .entry(NodeRef {
                session_id: sid(1),
                node: NodeId(6)
            })
            .unwrap()]),
        vec![3]
    );
    assert!(read.view(sid(1), 3).is_err());
}

#[test]
fn conversation_windows_count_older_turns_without_decoding_content() {
    for (damaged, field) in [
        (NodeId(3), "reply"),
        (NodeId(3), "attempts"),
        (NodeId(3), "slots"),
        (NodeId(3), "unexpected"),
        (NodeId(1), "message"),
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
        assert_eq!(entry_ordinals(&view.entries), vec![3]);
        let history = read.history(sid(1), NodeId(6), 1).unwrap();
        assert_eq!(entry_nodes(&history), vec![4, 5]);
        assert_eq!(entry_ordinals(&history), vec![2]);
        assert_eq!(
            entry_ordinals(&[read
                .entry(NodeRef {
                    session_id: sid(1),
                    node: NodeId(6),
                })
                .unwrap()]),
            vec![3]
        );
        assert!(matches!(
            read.view(sid(1), 3),
            Err(StoreError::Corruption { .. })
        ));
    }
}

#[test]
fn observer_projections_skip_unrelated_content() {
    for (node, pointer) in [
        (NodeId(2), "/reply"),
        (NodeId(2), "/slots"),
        (NodeId(2), "/attempts/0/usage"),
        (NodeId(2), "/attempts/0/progress/diagnostic"),
        (NodeId(1), "/message"),
        (NodeId(1), "/config_digest"),
        (NodeId(1), "/limits"),
    ] {
        let (_d, _l, db) = store();
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
        db.start_request(&lock, key(tid(1), 0, 1), origin(), now())
            .unwrap();
        edit_projection_node(&db, node, |wire| {
            *wire.pointer_mut(pointer).unwrap() = json!(false);
        });
        let read = db.begin_read().unwrap();
        assert!(
            !read
                .attempt_is_sent(sid(1), NodeId(2), key(tid(1), 0, 0))
                .unwrap()
        );
        assert!(
            read.attempt_is_sent(sid(1), NodeId(2), key(tid(1), 0, 1))
                .unwrap()
        );
        for other in [key(tid(2), 0, 1), key(tid(1), 1, 1), key(tid(1), 0, 2)] {
            assert!(!read.attempt_is_sent(sid(1), NodeId(2), other).unwrap());
        }
        assert_eq!(read.turn_agent(sid(1), tid(1)).unwrap(), "alpha");
        assert!(matches!(
            read.path_node(sid(1), node),
            Err(StoreError::Corruption { .. })
        ));
    }
}

fn edit_projection_node(db: &SessionStore, node: NodeId, edit: impl FnOnce(&mut Value)) {
    db.edit_test_node(
        NodeRef {
            session_id: sid(1),
            node,
        },
        edit,
    )
    .unwrap();
}

#[test]
fn usage_projection_skips_conversation_and_execution_material() {
    for position in [NodeId(2), NodeId(0)] {
        let (_d, _l, db) = store();
        let lock = db.try_lock_session(sid(1)).unwrap();
        accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
        db.start_request(&lock, key(tid(1), 0, 0), origin(), now())
            .unwrap();
        db.close_step(&lock, CloseStep::Finish(reply(key(tid(1), 0, 0), 0)), now())
            .unwrap();
        db.rewind(&lock, position, &project(1), now()).unwrap();
        let expected = db.begin_read().unwrap().usage(sid(1)).unwrap();
        assert_eq!(
            expected.path_total.attempts,
            u64::from(position != NodeId(0))
        );
        assert_eq!(expected.session_total.attempts, 1);
        for (node, pointers) in [
            (NodeId(1), &["/message", "/config_digest", "/limits"][..]),
            (
                NodeId(2),
                &[
                    "/reply",
                    "/slots",
                    "/progress",
                    "/attempts/0/origin",
                    "/attempts/0/progress",
                ][..],
            ),
        ] {
            edit_projection_node(&db, node, |wire| {
                for pointer in pointers {
                    *wire.pointer_mut(pointer).unwrap() = json!(false);
                }
            });
        }
        assert_eq!(db.begin_read().unwrap().usage(sid(1)).unwrap(), expected);
    }
}

#[test]
fn usage_projection_rejects_damaged_metering_fields() {
    for (node, pointer) in [
        (NodeId(1), "/provider"),
        (NodeId(1), "/model"),
        (NodeId(2), "/attempts"),
        (NodeId(2), "/attempts/0/usage"),
        (NodeId(2), "/attempts/0/usage_kind"),
        (NodeId(2), "/attempts/0/cost"),
    ] {
        let (_d, _l, db) = store();
        let lock = db.try_lock_session(sid(1)).unwrap();
        accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
        db.start_request(&lock, key(tid(1), 0, 0), origin(), now())
            .unwrap();
        edit_projection_node(&db, node, |wire| {
            *wire.pointer_mut(pointer).unwrap() = json!(false);
        });
        assert!(
            matches!(
                db.begin_read().unwrap().usage(sid(1)),
                Err(StoreError::Corruption { .. })
            ),
            "node={node:?} pointer={pointer}"
        );
    }
}

#[test]
fn observer_projections_reject_damaged_fields() {
    for (node, pointer) in [
        (NodeId(2), "/kind"),
        (NodeId(2), "/turn_id"),
        (NodeId(2), "/step_ordinal"),
        (NodeId(2), "/attempts"),
        (NodeId(2), "/attempts/0/progress"),
        (NodeId(2), "/attempts/0/progress/progress"),
        (NodeId(1), "/kind"),
        (NodeId(1), "/turn_id"),
        (NodeId(1), "/agent_name"),
    ] {
        for replacement in [None, Some(json!(false))] {
            let (_d, _l, db) = store();
            let lock = db.try_lock_session(sid(1)).unwrap();
            accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
            db.start_request(&lock, key(tid(1), 0, 0), origin(), now())
                .unwrap();
            edit_projection_node(&db, node, |wire| {
                let (parent, field) = pointer.rsplit_once('/').unwrap();
                let object = wire.pointer_mut(parent).unwrap().as_object_mut().unwrap();
                if let Some(value) = replacement {
                    object.insert(field.into(), value);
                } else {
                    object.remove(field);
                }
            });
            let read = db.begin_read().unwrap();
            let result = if node == NodeId(2) {
                read.attempt_is_sent(sid(1), node, key(tid(1), 0, 0))
                    .map(|_| ())
            } else {
                read.turn_agent(sid(1), tid(1)).map(|_| ())
            };
            assert!(
                matches!(result, Err(StoreError::Corruption { table, key, .. })
                if table == "nodes" && key == format!("{}/{}", sid(1), node.0)),
                "{pointer}"
            );
        }
    }
    for (node, field, value) in [
        (NodeId(2), "kind", json!("user_message")),
        (NodeId(1), "kind", json!("step")),
        (NodeId(1), "turn_id", json!(tid(2))),
    ] {
        let (_d, _l, db) = store();
        let lock = db.try_lock_session(sid(1)).unwrap();
        accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
        edit_projection_node(&db, node, |wire| wire[field] = value);
        let read = db.begin_read().unwrap();
        let result = if node == NodeId(2) {
            read.attempt_is_sent(sid(1), node, key(tid(1), 0, 0))
                .map(|_| ())
        } else {
            read.turn_agent(sid(1), tid(1)).map(|_| ())
        };
        assert!(matches!(result, Err(StoreError::Corruption { .. })));
    }
}

#[test]
fn conversation_counts_reject_damaged_headers() {
    for damaged in [NodeId(1), NodeId(3)] {
        for field in ["kind", "parent", "turn_id"] {
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
                    read.view(sid(1), 1).map(|_| ()),
                    read.history(sid(1), NodeId(6), 1).map(|_| ()),
                    read.entry(NodeRef {
                        session_id: sid(1),
                        node: NodeId(6),
                    })
                    .map(|_| ()),
                ] {
                    assert!(
                        matches!(result, Err(StoreError::Corruption { table, key, .. })
                        if table == "nodes" && key == format!("{}/{}", sid(1), damaged.0))
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
            read.view(sid(1), 1).map(|_| ()),
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
            for result in [
                read.usage(sid(1)).map(|_| ()),
                read.view(sid(1), 1).map(|_| ()),
                read.view(sid(1), 2).map(|_| ()),
                read.history(sid(1), NodeId(3), 1).map(|_| ()),
            ] {
                assert!(matches!(
                    result,
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
            } else {
                assert!(matches!(
                    entry,
                    Err(StoreError::Corruption { .. }) | Err(StoreError::UnsupportedVersion { .. })
                ));
            }
        }
    }
}

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
                let Node::UserMessage { parent, .. } =
                    r.nodes.get_mut(&(sid(1), NodeId(1))).unwrap()
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
            "turn_index_node" => r.turn_index.get_mut(&tid(1)).unwrap().node = NodeId(2),
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
    for case in ["user_provider", "consistent_invalid_provider"] {
        let raw = redb::Database::open(layout.lattice_root().join("lattice.redb")).unwrap();
        let tx = raw.begin_write().unwrap();
        overwrite_records(&tx, &base);
        {
            let mut nodes = tx.open_table(NODES).unwrap();
            let user_key = node_key(sid(1), NodeId(1));
            let mut user_wire: Value = {
                let value = nodes.get(user_key.as_slice()).unwrap().unwrap();
                serde_json::from_slice(&value.value()[4..]).unwrap()
            };
            user_wire["provider"] = json!("Bad_id");
            nodes
                .insert(user_key.as_slice(), encode(&user_wire).unwrap().as_slice())
                .unwrap();
            if case == "consistent_invalid_provider" {
                for n in [2, 3] {
                    let step_key = node_key(sid(1), NodeId(n));
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
fn value_versions_and_schema_rejections_name_the_database() {
    for table in [META, SESSIONS, SESSION_SETTINGS, CONFIGS, TURN_INDEX, INDEX] {
        let (_d, layout, db) = store();
        let (lock, _calls) = running(&db, sid(1), tid(1), 1, 1);
        drop(lock);
        drop(db);
        let raw = redb::Database::open(layout.lattice_root().join("lattice.redb")).unwrap();
        let tx = raw.begin_write().unwrap();
        {
            let mut t = tx.open_table(table).unwrap();
            let (key, mut bytes) = {
                let (key, value) = t.first().unwrap().unwrap();
                (key.value().to_string(), value.value().to_vec())
            };
            bytes[..4].copy_from_slice(&1u32.to_be_bytes());
            t.insert(key.as_str(), bytes.as_slice()).unwrap();
        }
        tx.commit().unwrap();
        drop(raw);
        assert!(
            matches!(SessionStore::open(&layout),Err(StoreError::UnsupportedVersion{path,found:1,supported:VERSION,..}) if path==layout.lattice_root().join("lattice.redb"))
        );
    }
    {
        let (_d, layout, db) = store();
        let (lock, _calls) = running(&db, sid(1), tid(1), 1, 1);
        drop(lock);
        drop(db);
        let raw = redb::Database::open(layout.lattice_root().join("lattice.redb")).unwrap();
        let tx = raw.begin_write().unwrap();
        {
            let mut t = tx.open_table(NODES).unwrap();
            let bytes = 1u32.to_be_bytes();
            t.insert(node_key(sid(1), NodeId(0)).as_slice(), bytes.as_slice())
                .unwrap();
        }
        tx.commit().unwrap();
        drop(raw);
        assert!(
            matches!(SessionStore::open(&layout),Err(StoreError::UnsupportedVersion{path,found:1,..}) if path==layout.lattice_root().join("lattice.redb"))
        );
    }
    let (_d, layout, db) = store();
    drop(db);
    corrupt_json(&layout, META, "store_kind", |v| *v = json!("wrong"));
    assert!(matches!(
        SessionStore::open(&layout),
        Err(StoreError::Corruption { .. })
    ));
}

#[test]
fn transaction_failure_matrix() {
    for phase in 0..13 {
        for point in [
            FaultPoint::Poisoned,
            FaultPoint::UnknownBefore,
            FaultPoint::UnknownAfter,
        ] {
            let (_dir, layout, db) = store();
            let lock = db.try_lock_session(sid(1)).unwrap();
            if phase != 11 {
                accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
            }
            if phase > 0 && !matches!(phase, 10..=12) {
                db.start_request(&lock, key(tid(1), 0, 0), origin(), now())
                    .unwrap();
            }
            let calls = if phase >= 4 && !matches!(phase, 8 | 10..=12) {
                Some(
                    db.register_calls(&lock, reply(key(tid(1), 0, 0), 2), &[iid(1), iid(2)], now())
                        .unwrap(),
                )
            } else {
                None
            };
            if phase >= 5 && !matches!(phase, 8 | 10..=12) {
                db.start_invocation(&lock, &calls.as_ref().unwrap().bindings[0], now())
                    .unwrap();
            }
            if phase >= 6 && !matches!(phase, 8 | 10..=12) {
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
                "phase {phase} point {point:?}: {result:?}"
            );
            assert!(before == codec::load_read(&read.tx).unwrap());
            let after = codec::load_read(&db.begin_read().unwrap().tx).unwrap();
            assert_eq!(
                before == after,
                point != FaultPoint::UnknownAfter,
                "phase {phase} point {point:?}"
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

// 损坏夹具绕过正常编码器的输入拒绝，让严格打开实际读到坏字节。
fn overwrite_records(tx: &redb::WriteTransaction, r: &Records) {
    fn bytes<T: serde::Serialize>(v: &T) -> Vec<u8> {
        let mut out = VERSION.to_be_bytes().to_vec();
        out.extend(canonical(v).unwrap());
        out
    }
    macro_rules! string_table {
        ($table:expr, $records:expr) => {{
            let mut t = tx.open_table($table).unwrap();
            t.retain(|_, _| false).unwrap();
            for (key, value) in $records {
                t.insert(key.to_string().as_str(), bytes(value).as_slice())
                    .unwrap();
            }
        }};
    }
    string_table!(SESSIONS, &r.sessions);
    string_table!(SESSION_SETTINGS, &r.session_settings);
    string_table!(TURN_INDEX, &r.turn_index);
    string_table!(INDEX, &r.index);
    {
        let mut t = tx.open_table(NODES).unwrap();
        t.retain(|_, _| false).unwrap();
        for ((s, n), v) in &r.nodes {
            t.insert(node_key(*s, *n).as_slice(), bytes(v).as_slice())
                .unwrap();
        }
    }
    {
        let mut t = tx.open_table(CONFIGS).unwrap();
        t.retain(|_, _| false).unwrap();
        for (k, v) in &r.configs {
            t.insert(config_key(k).as_str(), bytes(v).as_slice())
                .unwrap();
        }
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
    assert_eq!(
        entry_ordinals(&[read
            .entry(NodeRef {
                session_id: sid(1),
                node: NodeId(3)
            })
            .unwrap()]),
        vec![1]
    );
    assert!(read.history(sid(1), NodeId(3), 1).unwrap().is_empty());
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
        let user_id = NodeId(tip.0 - 1);
        let tx = db.db.begin_write().unwrap();
        let mut r = load_write(&tx).unwrap();
        let user = r.nodes.remove(&(sid(1), NodeId(1))).unwrap();
        let mut step_node = r.nodes.remove(&(sid(1), NodeId(2))).unwrap();
        let Node::Step { parent, .. } = &mut step_node else {
            unreachable!()
        };
        *parent = user_id;
        r.nodes.insert((sid(1), user_id), user);
        r.nodes.insert((sid(1), tip), step_node);
        r.turn_index.get_mut(&tid(1)).unwrap().node = user_id;
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

#[test]
fn json_payload_field_names_are_not_store_records() {
    let (_dir, layout, db) = store();
    let (lock, calls) = running(&db, sid(1), tid(1), 1, 1);
    let value = json!({"evidence":{"kind":"recorded","value":{"custom":true},"custom":true}});
    db.start_invocation(&lock, &calls.bindings[0], now())
        .unwrap();
    let terminal = prepare_terminal(&calls.bindings[0], ToolReply::Succeeded(value.clone()));
    db.finish_invocation(
        &lock,
        TerminalInput {
            invocation_id: iid(1),
            terminal,
        },
        now(),
    )
    .unwrap();
    db.close_step(&lock, CloseStep::Continue { key: calls.key }, now())
        .unwrap();
    drop(lock);
    drop(db);
    let db = SessionStore::open(&layout).unwrap();
    let content = node_at(&db, 2).step_content().unwrap();
    assert!(
        matches!(&content.results[0].payload, ResultPayload::Succeeded(SuccessContent::Json { value: actual, .. }) if *actual == value)
    );
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
    for case in ["user_identity", "index_address", "step_missing"] {
        let (_dir, _layout, db) = store();
        let (lock, _) = running(&db, sid(1), tid(1), 1, 1);
        let tx = db.db.begin_write().unwrap();
        let mut r = load_write(&tx).unwrap();
        match case {
            "user_identity" => {
                let Node::UserMessage { turn_id, .. } =
                    r.nodes.get_mut(&(sid(1), NodeId(1))).unwrap()
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

#[test]
fn draft_session_reopens_with_readable_projections_and_empty_configs() {
    let (_dir, layout, db) = store();
    let lock = db.try_lock_session(sid(1)).expect("获取会话执行锁");
    let mut draft_settings = SessionSettings::for_test(&config());
    draft_settings.system_prompt = Some("草稿内联系统提示".into());
    draft_settings.model = None;
    draft_settings.effort = None;
    let created = db
        .create_session(&lock, project(1), draft_settings.clone(), now())
        .expect("创建草稿会话");
    assert_eq!(created.session_id, sid(1), "草稿会话 ID 应一致");
    assert_eq!(created.current, NodeId(0), "草稿当前位置应为根节点 0");
    assert_eq!(created.created_at, now(), "created_at 应等于传入时间");
    assert_eq!(created.updated_at, now(), "updated_at 应等于 created_at");
    assert!(
        records(&db).configs.is_empty(),
        "草稿的系统提示内联保存，configs 表不能产生新条目"
    );
    drop(lock);
    drop(db);

    let reopened = SessionStore::open(&layout).expect("草稿会话应能通过严格打开");
    assert!(
        records(&reopened).configs.is_empty(),
        "重开后 configs 表仍应为空"
    );
    let read = reopened.begin_read().expect("开启只读事务");
    let head_record = read.session(sid(1)).expect("读取草稿 SessionRecord");
    assert_eq!(head_record, created, "重开后草稿会话头记录应保持一致");
    assert_eq!(
        read.path_node(sid(1), head_record.current)
            .expect("读取草稿当前路径根节点"),
        Node::Root,
        "草稿当前路径节点应为根节点"
    );
    assert_eq!(
        read.session_settings(sid(1)).expect("读取草稿会话设置"),
        draft_settings,
        "重开后草稿会话设置应保持一致"
    );
    let view = read.view(sid(1), 10).expect("读取草稿会话视图");
    assert_eq!(view.session, created);
    assert_eq!(view.current_agent, Some(agent("alpha")));
    assert!(view.entries.is_empty(), "草稿视图的对话条目应为空");

    let listed = read.list(None, 10).expect("读取草稿列表投影");
    assert_eq!(
        listed.items,
        vec![SessionListRow {
            session_id: sid(1),
            project_path: project(1),
            current: NodeId(0),
            node_count: 1,
            unclosed_turn: None,
            updated_at: now(),
        }],
        "列表应包含仅有 1 个根节点的草稿"
    );

    let log = read.log(sid(1)).expect("读取草稿日志投影");
    assert_eq!(log.session, created);
    assert_eq!(log.current_agent, Some(agent("alpha")));
    assert_eq!(log.unclosed_turn, None);
    assert_eq!(
        log.nodes,
        vec![LogRow {
            node: NodeHeader {
                id: NodeId(0),
                parent: None,
                turn_id: None,
                kind: NodeKind::Root,
            },
            current: true,
            turn_start: None,
        }],
        "日志投影应只包含当前根节点"
    );

    assert!(
        matches!(
            read.history(sid(1), NodeId(0), 10),
            Err(StoreError::InvalidInput {
                field: "before",
                ..
            })
        ),
        "草稿在根节点分页应按非用户消息边界返回 InvalidInput 而非损坏"
    );

    let usage = read.usage(sid(1)).expect("读取草稿用量投影");
    assert_eq!(usage.session_id, sid(1));
    assert_eq!(usage.position, NodeId(0));
    assert!(usage.turns.is_empty(), "草稿没有轮次用量");
    assert_eq!(usage.path_total.attempts, 0);
    assert_eq!(usage.session_total.attempts, 0);
}

#[test]
fn update_settings_preserves_frozen_turn_configs() {
    let (_dir, layout, db) = store();
    let lock = db.try_lock_session(sid(1)).expect("获取会话执行锁");
    let mut first_cfg = config();
    first_cfg.system_prompt = Some("第一轮冻结系统提示".into());
    first_cfg.effort = Some(lattice_conversation::Effort::Medium);
    let initial_settings = SessionSettings::for_test(&first_cfg);
    db.create_session(&lock, project(1), initial_settings.clone(), now())
        .expect("创建会话");
    let mut first_turn = accept(sid(1), tid(1));
    first_turn.config = first_cfg.clone();
    first_turn.settings_version = initial_settings.version().expect("计算初始设置版本");
    db.accept_turn(&lock, first_turn).expect("接受第一轮");

    let later = now() + chrono::Duration::seconds(30);
    let mut updated_settings = SessionSettings::for_test(&first_cfg);
    updated_settings.profile.agent_name = agent("beta");
    updated_settings.profile.description = Some("新 agent 描述".into());
    updated_settings.profile.timeout_secs = timeout(900);
    updated_settings.system_prompt = Some("修改后的下一轮系统提示".into());
    updated_settings.model = None;
    updated_settings.effort = None;

    assert!(
        matches!(
            db.update_settings(&lock, &project(2), updated_settings.clone(), later),
            Err(StoreError::BindingMismatch { session_id }) if session_id == sid(1)
        ),
        "项目绑定不一致时 update_settings 必须返回 BindingMismatch"
    );

    let updated_record = db
        .update_settings(&lock, &project(1), updated_settings.clone(), later)
        .expect("更新会话设置");
    assert_eq!(updated_record.created_at, now(), "created_at 必须保持不变");
    assert_eq!(
        updated_record.updated_at, later,
        "update_settings 必须刷新 updated_at"
    );
    assert_eq!(
        records(&db).configs.len(),
        1,
        "update_settings 不能向 configs 表追加配置条目"
    );
    drop(lock);
    drop(db);

    let reopened = SessionStore::open(&layout).expect("修改设置与最后一轮不同时仍可通过严格打开");
    let read = reopened.begin_read().expect("开启只读事务");
    assert_eq!(
        read.session_settings(sid(1)).expect("读取更新后的设置"),
        updated_settings,
        "重开后会话设置应为更新后的值"
    );
    assert_eq!(
        read.current_agent(sid(1)).expect("读取当前 agent"),
        Some(agent("beta")),
        "当前 agent 应取自更新后的会话设置"
    );
    assert_eq!(
        read.turn_config(sid(1), tid(1))
            .expect("读取第一轮冻结配置"),
        first_cfg,
        "已有轮次的冻结配置不能受 update_settings 影响"
    );
    assert_eq!(
        read.turn_detail(sid(1), tid(1))
            .expect("读取第一轮详情")
            .agent_name,
        agent("alpha"),
        "已有轮次节点上的 agent_name 必须保持第一轮冻结值"
    );
}

#[test]
fn accept_turn_rejects_mismatched_settings_version_without_writing_nodes() {
    let (_dir, layout, db) = store();
    let lock = db.try_lock_session(sid(1)).expect("获取会话执行锁");
    let initial_settings = SessionSettings::for_test(&config());
    db.create_session(&lock, project(1), initial_settings.clone(), now())
        .expect("创建草稿会话");

    let mut other_settings = initial_settings.clone();
    other_settings.effort = Some(lattice_conversation::Effort::High);
    let wrong_version = other_settings.version().expect("计算不同设置版本");

    let before_draft = records(&db);
    let mut stale_on_draft = accept(sid(1), tid(1));
    stale_on_draft.settings_version = wrong_version;
    let draft_err = db
        .accept_turn(&lock, stale_on_draft)
        .expect_err("草稿会话上版本不符应拒绝接受");
    assert!(
        matches!(draft_err, StoreError::SettingsChanged { session_id } if session_id == sid(1)),
        "预期返回 SettingsChanged，实际为 {draft_err:?}"
    );
    assert!(
        before_draft == records(&db),
        "草稿上 SettingsChanged 不能写入任何节点或配置"
    );

    let mut valid_first = accept(sid(1), tid(1));
    valid_first.settings_version = initial_settings.version().expect("计算初始版本");
    db.accept_turn(&lock, valid_first.clone())
        .expect("版本匹配时应正常接受第一轮");

    let later = now() + chrono::Duration::seconds(5);
    db.update_settings(&lock, &project(1), other_settings.clone(), later)
        .expect("更新会话设置");
    let before_second = records(&db);

    // 同一 TurnId 重放优先于设置版本核对，仍返回 Existing。
    assert!(
        matches!(
            db.accept_turn(&lock, valid_first),
            Ok(AcceptedTurn::Existing(detail)) if detail.turn_id == tid(1)
        ),
        "已接受 TurnId 重放在设置更新后仍应返回 Existing"
    );
    assert!(before_second == records(&db), "重放不能修改持久记录");

    let mut stale_second = accept(sid(1), tid(2));
    stale_second.settings_version = initial_settings.version().expect("计算旧版本");
    let second_err = db
        .accept_turn(&lock, stale_second)
        .expect_err("设置更新后携带旧版本应返回 SettingsChanged");
    assert!(
        matches!(second_err, StoreError::SettingsChanged { session_id } if session_id == sid(1)),
        "预期返回 SettingsChanged，实际为 {second_err:?}"
    );
    assert!(
        before_second == records(&db),
        "SettingsChanged 不能写入任何新节点或索引"
    );

    drop(lock);
    drop(db);
    assert!(
        before_second == records(&SessionStore::open(&layout).expect("重开会话库")),
        "重开后记录与拒绝前完全一致"
    );
}

#[test]
fn session_settings_strict_open_checks_shape_draft_and_bounds() {
    // 只有根节点的会话应当通过严格打开。
    {
        let (_dir, layout, db) = store();
        let lock = db.try_lock_session(sid(1)).expect("获取会话执行锁");
        let mut draft = SessionSettings::for_test(&config());
        draft.model = None;
        draft.effort = None;
        db.create_session(&lock, project(1), draft.clone(), now())
            .expect("创建只有根节点的会话");
        let expected = records(&db);
        drop(lock);
        drop(db);
        let reopened = SessionStore::open(&layout).expect("只有根节点的会话必须通过严格打开");
        assert!(expected == records(&reopened));
    }

    let (_dir, layout, db) = store();
    let lock = db.try_lock_session(sid(1)).expect("获取会话执行锁");
    accept_prepared(&db, &lock, accept(sid(1), tid(1))).expect("接受基准轮次");
    let base = records(&db);
    drop(lock);
    drop(db);

    for case in ["missing", "orphan", "oversized_prompt"] {
        let raw = redb::Database::open(layout.lattice_root().join("lattice.redb")).unwrap();
        let tx = raw.begin_write().unwrap();
        let mut changed = base.clone();
        match case {
            "missing" => {
                changed.session_settings.remove(&sid(1));
            }
            "orphan" => {
                changed
                    .session_settings
                    .insert(sid(2), base.session_settings[&sid(1)].clone());
            }
            "oversized_prompt" => {
                changed
                    .session_settings
                    .get_mut(&sid(1))
                    .unwrap()
                    .system_prompt = Some("a".repeat(2 * 1024 * 1024 + 1));
            }
            _ => unreachable!(),
        }
        overwrite_records(&tx, &changed);
        tx.commit().unwrap();
        drop(raw);
        assert!(
            matches!(
                SessionStore::open(&layout),
                Err(StoreError::Corruption { .. })
            ),
            "严格打开必须拒绝损坏或自身无效的设置（case={case}）"
        );
    }

    for case in [
        "unknown",
        "legacy_profile_field",
        "effort",
        "agent_name",
        "default_model",
        "timeout_secs",
        "retry",
        "model",
        "version",
        "key",
        "canonical",
        "truncated",
        "table",
    ] {
        let raw = redb::Database::open(layout.lattice_root().join("lattice.redb")).unwrap();
        let tx = raw.begin_write().unwrap();
        overwrite_records(&tx, &base);
        if case == "table" {
            tx.delete_table(SESSION_SETTINGS).unwrap();
        } else {
            let mut table = tx.open_table(SESSION_SETTINGS).unwrap();
            let mut value = serde_json::to_value(&base.session_settings[&sid(1)]).unwrap();
            match case {
                "unknown" => value["unknown"] = json!(true),
                "legacy_profile_field" => value["profile"]["enabled_tools"] = json!(["observe"]),
                "effort" => value["effort"] = json!("default"),
                "agent_name" => value["profile"]["agent_name"] = json!("../bad"),
                "default_model" => value["profile"]["default_model"] = json!("missing-slash"),
                "timeout_secs" => value["profile"]["timeout_secs"] = json!(0),
                "retry" => value["profile"]["retry"]["max_retries"] = json!(11),
                "model" => value["model"] = json!("Bad_id/gpt-5"),
                _ => {}
            }
            let mut bytes = encode(&value).unwrap();
            if case == "version" {
                bytes[..4].copy_from_slice(&7_u32.to_be_bytes());
            }
            if case == "canonical" {
                bytes.push(b' ');
            }
            if case == "truncated" {
                bytes.truncate(3);
            }
            table
                .insert(sid(1).to_string().as_str(), bytes.as_slice())
                .unwrap();
            if case == "key" {
                table
                    .insert("invalid", encode(&value).unwrap().as_slice())
                    .unwrap();
            }
        }
        tx.commit().unwrap();
        drop(raw);
        let reopened = SessionStore::open(&layout).map(|_| ());
        if case == "version" {
            assert!(
                matches!(
                    reopened,
                    Err(StoreError::UnsupportedVersion {
                        found: 7,
                        supported: VERSION,
                        ..
                    })
                ),
                "{case}: {reopened:?}"
            );
        } else {
            assert!(
                matches!(reopened, Err(StoreError::Corruption { .. })),
                "{case}: {reopened:?}"
            );
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

#[test]
fn non_utf8_project_path_persists_across_reopen() {
    let (_dir, layout, db) = store();
    let non_utf8 = ProjectPath::from_bytes(b"/tmp/proj-\xff\xfe/sub".as_slice())
        .expect("构造非 UTF-8 项目路径");
    let lock = db.try_lock_session(sid(1)).expect("获取会话锁");
    let mut input = accept(sid(1), tid(1));
    input.project_path = non_utf8.clone();
    accept_prepared(&db, &lock, input).expect("写入非 UTF-8 项目路径会话");
    drop(lock);
    drop(db);

    let reopened = SessionStore::open(&layout).expect("重新打开会话库");
    let read = reopened.begin_read().expect("开启只读事务");
    let session = read.session(sid(1)).expect("读取会话记录");
    assert_eq!(
        session.project_path, non_utf8,
        "重开后读取 SessionRecord 的非 UTF-8 项目路径应保持不变"
    );
    let listed = read.list(None, 10).expect("列出会话");
    assert_eq!(
        listed.items[0].project_path, non_utf8,
        "重开后会话列表投影中的非 UTF-8 项目路径应保持不变"
    );
    let log = read.log(sid(1)).expect("读取会话日志");
    assert_eq!(
        log.session.project_path, non_utf8,
        "重开后会话日志中的非 UTF-8 项目路径应保持不变"
    );
}
