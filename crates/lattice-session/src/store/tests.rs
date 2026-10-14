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
    db.accept_turn(&lock, accept(sid(1), tid(1))).unwrap();
    db.start_request(&lock, key(tid(1), 0, 0), now()).unwrap();
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
                matches!(&resolve_slot(&slots[0]).0.payload, ResultPayload::Succeeded(SuccessContent::Json { value: actual, .. }) if *actual == value)
            );
        }
    }
    drop(lock);
    drop(db);
    SessionStore::open(&layout).unwrap();
}

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
                .turn_node(tid(1))
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
    for version in [0, 1, VERSION + 1, u32::MAX] {
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
    let (_lock, calls) = running(&db, sid(1), tid(1), 1, 1);
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
    let wire = serde_json::to_value(r.changes[&ChangeSeq(1)]).unwrap();
    assert_eq!(
        wire,
        json!({"sequence":1,"invocation_id":calls.bindings[0].invocation_id,"stage":"registered"})
    );
    assert_eq!(
        decode::<StoredInvocationChange>(
            "invocation_changes",
            "1",
            &encode(&r.changes[&ChangeSeq(1)]).unwrap()
        )
        .unwrap(),
        r.changes[&ChangeSeq(1)]
    );
}

#[test]
fn nested_unknown_fields_are_rejected() {
    let (_d, _l, db) = store();
    let (lock, calls) = running(&db, sid(1), tid(1), 1, 1);
    finish(
        &db,
        &lock,
        &calls.bindings[0],
        Some(CepErrorCode::ExecutionFailed),
    );
    let original = serde_json::to_value(node_at(&db, 2)).unwrap();
    for pointer in [
        "/attempts/0/usage",
        "/attempts/0/progress",
        "/attempts/0/progress/end/step",
        "/reply",
        "/reply/parts/1",
        "/reply/parts/1/semantic/tool_call/call",
        "/slots/0",
        "/slots/0/binding",
        "/slots/0/binding/identity",
        "/slots/0/progress",
        "/slots/0/progress/result/payload/payload",
        "/slots/0/progress/evidence",
        "/slots/0/progress/evidence/value",
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
    let mut extended = original;
    extended
        .pointer_mut("/slots/0/progress/evidence/value/admission")
        .unwrap()["future"] = json!({"nested":true});
    assert!(decode::<Node>("nodes", "2", &encode(&extended).unwrap()).is_ok());
    let effect = json!({"seq":0,"cell":{"ctx_op":"alloc","ext_impact":"none","scope":"workspace"},"scope":{"scope":"workspace","trust":"in","channel":"workspace"},"target":{"kind":"path","path":"a"},"source":"operation","future":true});
    extended
        .pointer_mut("/slots/0/progress/evidence/value/effects/effects")
        .unwrap()
        .as_array_mut()
        .unwrap()
        .push(effect);
    assert!(decode::<Node>("nodes", "2", &encode(&extended).unwrap()).is_ok());
    extended
        .pointer_mut("/slots/0/progress/evidence/value/effects/effects/0/cell")
        .unwrap()["unknown"] = json!(true);
    assert!(decode::<Node>("nodes", "2", &encode(&extended).unwrap()).is_err());
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
    for (slot, payload, reason) in [
        (
            &registered,
            ResultPayload::NotExecuted,
            EvidenceUnavailableReason::NotDispatched,
        ),
        (
            &started,
            ResultPayload::Indeterminate(IndeterminateCause::NoResult),
            EvidenceUnavailableReason::NoResult,
        ),
    ] {
        assert_eq!(
            resolve_slot(slot),
            (
                StoredToolResult {
                    invocation_id: iid(1),
                    name: "observe.workspace".into(),
                    payload
                },
                InvocationEvidence::Unavailable { reason }
            )
        );
    }
    for code in [
        None,
        Some(CepErrorCode::ExecutionFailed),
        Some(CepErrorCode::CallCancelled),
        Some(CepErrorCode::OutcomeIndeterminate),
    ] {
        let term = terminal(&calls.bindings[0], code);
        let slot = InvocationSlot {
            binding: calls.bindings[0].clone(),
            progress: SlotProgress::Terminal {
                result: term.result.clone(),
                evidence: term.evidence.clone(),
            },
        };
        assert_eq!(
            resolve_slot(&slot),
            (term.result.clone(), term.evidence.clone())
        );
        for stop in [
            DispatchStop::Cancelled,
            DispatchStop::Deadline,
            DispatchStop::ProcessExit,
        ] {
            let expected = match code {
                Some(CepErrorCode::CallCancelled) => Some(StopCause::Cancelled),
                Some(CepErrorCode::OutcomeIndeterminate) => {
                    Some(StopCause::InvocationIndeterminate {
                        invocation_id: iid(1),
                    })
                }
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
        attempt: 0,
        origin: ProviderOrigin::OpenAiCompatible,
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
        AttemptEnd::Completed {
            step: calls.bindings[0].identity.address().message,
        },
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
    let mut bad = started.clone();
    bad.binding = calls.bindings[1].clone();
    assert!(step_stop_cause(&[registered, bad], DispatchStop::Deadline).is_err());
}

#[test]
fn acceptance_deduplicates_configs_and_preserves_abandoned_nodes() {
    let (_d, layout, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    db.accept_turn(&lock, accept(sid(1), tid(1))).unwrap();
    let old = node_at(&db, 2);
    let mut next = accept(sid(1), tid(2));
    next.session = AcceptSession::Existing { id: sid(1) };
    next.config.agent_name = "beta".into();
    db.accept_turn(&lock, next).unwrap();
    assert_eq!(records(&db).configs.len(), 1);
    assert_eq!(node_at(&db, 2), old);
    assert_eq!(
        db.begin_read()
            .unwrap()
            .current_agent(sid(1))
            .unwrap()
            .as_deref(),
        Some("beta")
    );
    assert!(matches!(
        db.start_request(&lock, key(tid(1), 0, 0), now()),
        Err(StoreError::InvalidTransition { .. })
    ));
    let mut retry = accept(sid(2), tid(1));
    retry.config.agent_name.clear();
    let lock2 = db.try_lock_session(sid(2)).unwrap();
    assert!(
        matches!(db.accept_turn(&lock2,retry).unwrap(),AcceptedTurn::Existing(t) if t.session_id==sid(1))
    );
    for case in 0..3 {
        let mut conflicting = accept(sid(1), tid(1));
        match case {
            0 => conflicting.input.push('!'),
            1 => conflicting.project_store_key = project(2),
            _ => conflicting.session = AcceptSession::Existing { id: sid(2) },
        }
        assert!(matches!(
            db.accept_turn(if case == 2 { &lock2 } else { &lock }, conflicting),
            Err(StoreError::Conflict { .. })
        ));
    }
    db.rewind(&lock, NodeId(0), now()).unwrap();
    let mut next = accept(sid(1), tid(3));
    next.session = AcceptSession::Existing { id: sid(1) };
    next.agent_selection = AgentSelection::Inherited;
    assert!(db.accept_turn(&lock, next.clone()).is_err());
    next.agent_selection = AgentSelection::Explicit;
    next.config.system_prompt = Some("different".into());
    db.accept_turn(&lock, next).unwrap();
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
fn invalid_acceptance_and_lock_capabilities_leave_no_facts() {
    let (_d, _l, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    for case in 0..13 {
        let mut a = accept(sid(1), tid(1));
        match case {
            0 => a.input = "x".repeat(MAX_USER_INPUT_BYTES + 1),
            1 => a.agent_selection = AgentSelection::Inherited,
            2 => a.config.system_prompt = Some("x".repeat(2 * 1024 * 1024 + 1)),
            3 => a.config.provider = "unknown".into(),
            4 => a
                .config
                .catalog
                .entries
                .push(a.config.catalog.entries[0].clone()),
            5 => a.config.limits.max_model_steps = 33,
            6 => a.config.limits.max_model_attempts = 0,
            7 => a.config.agent_name.clear(),
            8 => a.config.catalog.digest = [0; 32],
            9 => a.config.limits.timeout_secs = 0,
            10 => a.config.limits.max_model_attempts = 12,
            11 => a.config.model.clear(),
            _ => a.config.catalog.entries[0].descriptor_identity = "A".repeat(64),
        }
        assert!(db.accept_turn(&lock, a).is_err(), "{case}");
        assert!(records(&db).nodes.is_empty());
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
        db2.accept_turn(&lock, accept(sid(1), tid(1))),
        Err(StoreError::InvalidLockCapability)
    ));
    let mut a = accept(sid(1), tid(1));
    a.input = "x".repeat(MAX_USER_INPUT_BYTES);
    db.accept_turn(&lock, a).unwrap();
    assert!(matches!(
        db.accept_turn(&lock, accept(sid(1), tid(2))),
        Err(StoreError::SessionCollision { .. })
    ));
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
        db.accept_turn(&lock, accept(sid(1), tid(1))).unwrap();
        if cut >= 1 {
            db.start_request(&lock, key(tid(1), 0, 0), now()).unwrap();
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
                            (cut == 8).then_some(CepErrorCode::CallCancelled),
                        ),
                    },
                    now(),
                )
                .unwrap();
            }
            if cut >= 9 {
                finish(&db, &lock, &calls.bindings[1], None);
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
                    Some(OperatorReason::Indeterminate(IndeterminateCause::NoResult))
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
        read.snapshot(sid(1), None).unwrap();
        read.log(sid(1)).unwrap();
        read.list(None, 10).unwrap();
        read.query_audit(&filter(project(1)), None, 100).unwrap();
        read.tail_audit(&filter(project(1)), ChangeSeq(0), 100)
            .unwrap();
        assert!(before == records(&db), "read changed cut {cut}");
        drop(read);
        let lock = db.try_lock_session(sid(1)).unwrap();
        db.rewind(&lock, NodeId(0), now()).unwrap();
        let mut next = accept(sid(1), tid(2));
        next.session = AcceptSession::Existing { id: sid(1) };
        db.accept_turn(&lock, next).unwrap();
        assert_eq!(detail(&db), expected, "old turn changed at cut {cut}");
    }
}

#[test]
fn retries_usage_and_between_request_stops() {
    for cause in [StopCause::Cancelled, StopCause::Deadline] {
        let (_d, _l, db) = store();
        let lock = db.try_lock_session(sid(1)).unwrap();
        db.accept_turn(&lock, accept(sid(1), tid(1))).unwrap();
        db.start_request(&lock, key(tid(1), 0, 0), now()).unwrap();
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
            prompt_tokens: 1,
            completion_tokens: 2,
            total_tokens: 3,
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
    db.accept_turn(&lock, accept(sid(1), tid(1))).unwrap();
    for a in 0..3 {
        db.start_request(&lock, key(tid(1), 0, a), now()).unwrap();
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
        t.usage,
        UsageSummary::Partial {
            known: TokenUsage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0
            },
            unknown_attempts: 3
        }
    );
    assert_eq!(
        t.summary.notice.unwrap(),
        Notice {
            level: NoticeLevel::Error,
            text: "old provider diagnostic".into()
        }
    );
    assert!(db.start_request(&lock, key(tid(1), 0, 3), now()).is_err());
}

#[test]
fn step_budget_reserves_and_closes_limit_node() {
    let (_d, layout, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    db.accept_turn(&lock, accept(sid(1), tid(1))).unwrap();
    for ordinal in 0..MAX_MODEL_STEPS {
        db.start_request(&lock, key(tid(1), ordinal, 0), now())
            .unwrap();
        let calls = db
            .register_calls(
                &lock,
                reply(key(tid(1), ordinal, 0), 1),
                &[iid(u64::from(ordinal) + 1)],
                now(),
            )
            .unwrap();
        finish(&db, &lock, &calls.bindings[0], None);
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
        db.start_request(&lock, key(tid(1), MAX_MODEL_STEPS, 0), now())
            .is_err()
    );
    assert_eq!(
        t.usage,
        UsageSummary::Complete {
            total: TokenUsage {
                prompt_tokens: 64,
                completion_tokens: 96,
                total_tokens: 160
            }
        }
    );
    drop(lock);
    drop(db);
    assert_eq!(detail(&SessionStore::open(&layout).unwrap()), t);
}

#[test]
fn snapshots_history_retries_and_rewind_are_stable() {
    let (_d, _l, db) = store();
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
    let snapshot = db.begin_read().unwrap().snapshot(sid(1), None).unwrap();
    assert_eq!(snapshot.history.head, NodeId(1));
    assert_eq!(snapshot.unclosed_step, Some((NodeId(2), node_at(&db, 2))));
    assert!(matches!(
        db.begin_read().unwrap().history_page(
            &HistoryAnchor {
                session_id: sid(1),
                head: NodeId(2)
            },
            None,
            100
        ),
        Err(StoreError::InvalidCursor)
    ));
    let old_read = db.begin_read().unwrap();
    let old_node = node_at(&db, 2);
    db.start_request(&lock, key(tid(1), 0, 1), now()).unwrap();
    db.close_step(&lock, CloseStep::Finish(reply(key(tid(1), 0, 1), 0)), now())
        .unwrap();
    assert_eq!(
        old_read
            .node(NodeRef {
                session_id: sid(1),
                node: NodeId(2)
            })
            .unwrap(),
        old_node
    );
    assert_eq!(history(&db, &snapshot.history, 63).len(), 1);
    let closed = db.begin_read().unwrap().snapshot(sid(1), None).unwrap();
    assert!(closed.unclosed_step.is_none());
    let page = db
        .begin_read()
        .unwrap()
        .history_page(&closed.history, None, 10000)
        .unwrap();
    assert_eq!(page.fragments.len(), 2);
    assert_eq!(page.fragments[1].retries.len(), 1);
    assert_eq!(page.fragments[1].retries[0].notice.level, NoticeLevel::Warn);
    assert_eq!(
        history(&db, &closed.history, 63),
        vec![node_at(&db, 1), node_at(&db, 2)]
    );
    let before = records(&db);
    db.rewind(&lock, NodeId(1), now()).unwrap();
    assert_eq!(records(&db).nodes, before.nodes);
    assert_eq!(
        db.begin_read()
            .unwrap()
            .current_agent(sid(1))
            .unwrap()
            .as_deref(),
        Some("alpha")
    );
    assert!(db.rewind(&lock, NodeId(99), now()).is_err());
    let bad = HistoryCursor {
        anchor: closed.history.clone(),
        node: NodeId(2),
        utf8_offset: u64::MAX,
    };
    assert!(matches!(
        db.begin_read()
            .unwrap()
            .history_page(&closed.history, Some(&bad), 64),
        Err(StoreError::InvalidCursor)
    ));
    let mut bad = bad;
    bad.node = NodeId(1);
    bad.anchor.head = NodeId(1);
    assert!(matches!(
        db.begin_read()
            .unwrap()
            .history_page(&closed.history, Some(&bad), 64),
        Err(StoreError::InvalidCursor)
    ));
}

#[test]
fn audit_resolves_terminal_from_nodes_without_close_rows() {
    let (_d, layout, db) = store();
    let (lock, calls) = running(&db, sid(1), tid(1), 2, 1);
    let before = db.begin_read().unwrap();
    finish(
        &db,
        &lock,
        &calls.bindings[0],
        Some(CepErrorCode::CallCancelled),
    );
    let read = db.begin_read().unwrap();
    let tail = read
        .tail_audit(&filter(project(1)), ChangeSeq(0), 100)
        .unwrap();
    assert_eq!(tail.rows.len(), 4);
    assert!(matches!(
        tail.rows[0].row.state,
        OperatorInvocationState::Registered
    ));
    assert!(matches!(
        tail.rows[2].row.state,
        OperatorInvocationState::Started
    ));
    assert!(matches!(
        tail.rows[3].row.state,
        OperatorInvocationState::Terminal {
            outcome: ToolResultOutcome::Indeterminate,
            ..
        }
    ));
    assert_eq!(
        before
            .tail_audit(&filter(project(1)), ChangeSeq(0), 100)
            .unwrap()
            .rows
            .len(),
        2
    );
    assert!(
        db.start_invocation(&lock, &calls.bindings[1], now())
            .is_err()
    );
    db.close_step(
        &lock,
        CloseStep::StopExecuting {
            key: step(tid(1), 0),
            stop: DispatchStop::Deadline,
        },
        now(),
    )
    .unwrap();
    assert_eq!(detail(&db).summary.outcome, PublicTurnOutcome::Cancelled);
    assert_eq!(records(&db).last_change, ChangeSeq(4));
    let current = db.begin_read().unwrap();
    assert_eq!(
        current
            .query_audit(&filter(project(1)), None, 1)
            .unwrap()
            .next,
        Some(iid(1))
    );
    assert_eq!(
        current
            .query_audit(&filter(project(1)), Some(iid(1)), 1)
            .unwrap()
            .rows[0]
            .state,
        OperatorInvocationState::Registered
    );
    let mut cursor = ChangeSeq(0);
    let mut rows = Vec::new();
    loop {
        let p = current.tail_audit(&filter(project(1)), cursor, 1).unwrap();
        rows.extend(p.rows);
        cursor = p.next_after;
        if p.done {
            break;
        }
    }
    assert_eq!(rows, tail.rows);
    let other = filter(project(2));
    let p = current.tail_audit(&other, ChangeSeq(0), 100).unwrap();
    assert!(p.rows.is_empty());
    assert!(p.done);
    assert!(current.tail_audit(&other, ChangeSeq(5), 1).is_err());
    assert!(current.query_audit(&other, None, 0).is_err());
    let replay = current.replay_audit(&project(1), sid(1), tid(1)).unwrap();
    assert_eq!(replay.calls.len(), 2);
    drop(current);
    drop(read);
    drop(before);
    drop(lock);
    drop(db);
    SessionStore::open(&layout).unwrap();
}
fn corruptible_fixture(db: &SessionStore) -> SessionExecutionLock {
    let (lock, calls) = running(db, sid(1), tid(1), 1, 1);
    finish(db, &lock, &calls.bindings[0], None);
    db.close_step(
        &lock,
        CloseStep::Continue {
            key: step(tid(1), 0),
        },
        now(),
    )
    .unwrap();
    db.start_request(&lock, key(tid(1), 1, 0), now()).unwrap();
    db.stop_attempt(
        &lock,
        stopped(
            key(tid(1), 1, 0),
            StopCause::ProviderError { retryable: true },
        ),
        now(),
    )
    .unwrap();
    db.start_request(&lock, key(tid(1), 1, 1), now()).unwrap();
    let calls = db
        .register_calls(
            &lock,
            reply(key(tid(1), 1, 1), 3),
            &[iid(2), iid(3), iid(4)],
            now(),
        )
        .unwrap();
    finish(db, &lock, &calls.bindings[0], None);
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
        "user_provider",
        "user_limits",
        "user_message",
        "intermediate_unclosed",
        "last_continue",
        "executing_reply",
        "reply_binding",
        "reply_provider",
        "reply_slot_count",
        "slot_turn",
        "terminal_not_executed",
        "terminal_no_result",
        "terminal_without_receipt",
        "slot_order",
        "duplicate_invocation",
        "evidence_version",
        "result_digest",
        "attempt_order",
        "attempt_origin",
        "completed_address",
        "retry_on_complete",
        "retry_missing",
        "retry_diagnostic",
        "sent_usage",
        "index_missing",
        "index_extra",
        "index_address",
        "index_last",
        "change_gap",
        "change_sequence",
        "change_lifecycle",
        "change_orphan",
        "high_water",
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
            "user_provider" | "user_limits" | "user_message" => {
                let Node::UserMessage {
                    provider,
                    limits,
                    message,
                    ..
                } = r.nodes.get_mut(&(sid(1), NodeId(1))).unwrap()
                else {
                    unreachable!()
                };
                match case {
                    "user_provider" => *provider = "unknown".into(),
                    "user_limits" => limits.max_model_steps = 1,
                    _ => message.role = MessageRole::Assistant,
                }
            }
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
            "index_last" => r.index.get_mut(&iid(2)).unwrap().last_change = ChangeSeq(1),
            "change_gap" => {
                r.changes.remove(&ChangeSeq(1));
            }
            "change_sequence" => r.changes.get_mut(&ChangeSeq(1)).unwrap().sequence = ChangeSeq(99),
            "change_lifecycle" => {
                r.changes.get_mut(&ChangeSeq(1)).unwrap().stage = InvocationStage::Started
            }
            "change_orphan" => r.changes.get_mut(&ChangeSeq(1)).unwrap().invocation_id = iid(99),
            "high_water" => r.last_change = ChangeSeq(99),
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
                    "reply_provider" => {
                        reply.as_mut().unwrap().provider_origin = Some(ProviderOrigin::Gemini)
                    }
                    "reply_slot_count" => {
                        slots.pop();
                    }
                    "slot_turn" => slots[0].binding.identity.turn_id = tid(99),
                    "terminal_not_executed" | "terminal_no_result" | "terminal_without_receipt" => {
                        let SlotProgress::Terminal { result, evidence } = &mut slots[0].progress
                        else {
                            unreachable!()
                        };
                        result.payload = match case {
                            "terminal_not_executed" => ResultPayload::NotExecuted,
                            "terminal_no_result" => {
                                ResultPayload::Indeterminate(IndeterminateCause::NoResult)
                            }
                            _ => ResultPayload::Indeterminate(IndeterminateCause::Reported),
                        };
                        *evidence = InvocationEvidence::Unavailable {
                            reason: if case == "terminal_not_executed" {
                                EvidenceUnavailableReason::NotDispatched
                            } else {
                                EvidenceUnavailableReason::NoResult
                            },
                        };
                    }
                    "slot_order" => {
                        slots[2].progress = SlotProgress::Started;
                    }
                    "duplicate_invocation" => slots[2].binding.invocation_id = iid(2),
                    "evidence_version" => {
                        let SlotProgress::Terminal {
                            evidence: InvocationEvidence::Recorded(e),
                            ..
                        } = &mut slots[0].progress
                        else {
                            unreachable!()
                        };
                        e.effects.version += 1;
                    }
                    "result_digest" => {
                        let SlotProgress::Terminal { result, .. } = &mut slots[0].progress else {
                            unreachable!()
                        };
                        let ResultPayload::Succeeded(SuccessContent::Json { digest, .. }) =
                            &mut result.payload
                        else {
                            unreachable!()
                        };
                        *digest = [0; 32];
                    }
                    "attempt_order" => attempts[0].attempt = 2,
                    "attempt_origin" => attempts[0].origin = ProviderOrigin::Anthropic,
                    "completed_address" => {
                        let AttemptProgress::Ended {
                            end: AttemptEnd::Completed { step },
                            ..
                        } = &mut attempts[1].progress
                        else {
                            unreachable!()
                        };
                        step.session_id = sid(2);
                    }
                    "retry_on_complete" => attempts[1].retry_delay_ms = Some(1),
                    "retry_missing" => attempts[0].retry_delay_ms = None,
                    "retry_diagnostic" => {
                        let AttemptProgress::Ended { diagnostic, .. } = &mut attempts[0].progress
                        else {
                            unreachable!()
                        };
                        diagnostic.provider_error = None;
                    }
                    "sent_usage" => attempts[1].progress = AttemptProgress::Sent,
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
}

#[test]
fn local_reads_detect_live_corruption_and_open_checks_unrelated_sessions() {
    for case in 0..7 {
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
            4 => r.changes.get_mut(&ChangeSeq(8)).unwrap().invocation_id = iid(99),
            5 => {
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
        if case == 6 {
            read.turn_detail(sid(1), tid(1)).unwrap();
        } else {
            assert!(
                read.turn_detail(sid(1), tid(1)).is_err(),
                "detail case {case}"
            );
            assert!(read.snapshot(sid(1), None).is_err(), "snapshot case {case}");
            assert!(
                read.query_audit(&filter(project(1)), None, 100).is_err(),
                "audit case {case}"
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
    for table in [META, SESSIONS, CONFIGS, TURN_INDEX, INDEX] {
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
    for table in ["nodes", "invocation_changes"] {
        let (_d, layout, db) = store();
        let (lock, _calls) = running(&db, sid(1), tid(1), 1, 1);
        drop(lock);
        drop(db);
        let raw = redb::Database::open(layout.lattice_root().join("lattice.redb")).unwrap();
        let tx = raw.begin_write().unwrap();
        if table == "nodes" {
            let mut t = tx.open_table(NODES).unwrap();
            let bytes = 1u32.to_be_bytes();
            t.insert(node_key(sid(1), NodeId(0)).as_slice(), bytes.as_slice())
                .unwrap();
        } else {
            let mut t = tx.open_table(CHANGES).unwrap();
            let bytes = 1u32.to_be_bytes();
            t.insert(1, bytes.as_slice()).unwrap();
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
fn tail_rejects_invalid_lifecycle_across_pages() {
    let mut missed = Vec::new();
    for case in ["repeated_registered", "missing_started", "repeated_started"] {
        let (_dir, layout, db) = store();
        let (lock, calls) = running(&db, sid(1), tid(1), 1, 1);
        let (_other, _) = running(&db, sid(2), tid(2), 1, 2);
        finish(&db, &lock, &calls.bindings[0], None);

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
                records.changes.get_mut(&ChangeSeq(3)).unwrap().stage = InvocationStage::Registered;
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
                let mut started = records.changes[&ChangeSeq(3)];
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

#[test]
fn registration_rejects_corrupt_change_high_water() {
    for high_water in [0, 4] {
        let (_dir, _layout, db) = store();
        let (first, calls) = running(&db, sid(1), tid(1), 1, 1);
        finish(&db, &first, &calls.bindings[0], None);

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

#[test]
fn change_publication_rejects_occupied_sequence() {
    let (_dir, _layout, db) = store();
    let (_lock, _) = running(&db, sid(1), tid(1), 1, 1);
    let original = load_read(&db.begin_read().unwrap().tx).unwrap();
    let mut before = original.clone();
    before.changes.clear();
    let mut after = before.clone();
    let mut conflicting = original.changes[&ChangeSeq(1)];
    conflicting.invocation_id = iid(99);
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

#[test]
fn transaction_failure_matrix() {
    for phase in 0..11 {
        for point in [FaultPoint::UnknownBefore, FaultPoint::UnknownAfter] {
            let (_dir, layout, db) = store();
            let lock = db.try_lock_session(sid(1)).unwrap();
            db.accept_turn(&lock, accept(sid(1), tid(1))).unwrap();
            if phase > 0 && phase != 10 {
                db.start_request(&lock, key(tid(1), 0, 0), now()).unwrap();
            }
            let calls = if phase >= 4 && phase != 8 && phase != 10 {
                Some(
                    db.register_calls(&lock, reply(key(tid(1), 0, 0), 2), &[iid(1), iid(2)], now())
                        .unwrap(),
                )
            } else {
                None
            };
            if phase >= 5 && phase != 8 && phase != 10 {
                db.start_invocation(&lock, &calls.as_ref().unwrap().bindings[0], now())
                    .unwrap();
            }
            if phase >= 6 && phase != 8 && phase != 10 {
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
            if phase == 9 {
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
                10 => {
                    let mut next = accept(sid(1), tid(2));
                    next.session = AcceptSession::Existing { id: sid(1) };
                    db.accept_turn(&lock, next).map(|_| ())
                }
                7 => db.rewind(&lock, NodeId(0), now()).map(|_| ()),
                8 => db
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
    let mut bytes = VERSION.to_be_bytes().to_vec();
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

    drop(lock);
    drop(db);
    let db = SessionStore::open(&layout).unwrap();
    let Node::Step { slots, .. } = node_at(&db, 2) else {
        unreachable!()
    };
    assert_eq!(resolve_slot(&slots[0]), (term.result, term.evidence));
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
    {
        let mut t = tx.open_table(CHANGES).unwrap();
        t.retain(|_, _| false).unwrap();
        for (k, v) in &r.changes {
            t.insert(k.0, bytes(v).as_slice()).unwrap();
        }
    }
    tx.open_table(META)
        .unwrap()
        .insert("last_change_seq", bytes(&r.last_change).as_slice())
        .unwrap();
}
#[test]
fn ordinary_operations_load_only_referenced_turns() {
    let (_d, layout, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    db.accept_turn(&lock, accept(sid(1), tid(1))).unwrap();
    db.rewind(&lock, NodeId(0), now()).unwrap();
    let mut next = accept(sid(1), tid(2));
    next.session = AcceptSession::Existing { id: sid(1) };
    db.accept_turn(&lock, next).unwrap();
    let other = db.try_lock_session(sid(2)).unwrap();
    db.accept_turn(&other, accept(sid(2), tid(3))).unwrap();
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
    assert_eq!(read.turn_detail(sid(1), tid(2)).unwrap().steps.len(), 1);
    let snapshot = read.snapshot(sid(1), None).unwrap();
    assert_eq!(
        read.history_page(&snapshot.history, None, 4096)
            .unwrap()
            .fragments
            .len(),
        1
    );
    assert!(read.turn_node(tid(1)).is_err());
    db.start_request(&lock, key(tid(2), 0, 0), now()).unwrap();
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
    db.accept_turn(&lock, accept(sid(1), tid(1))).unwrap();
    assert!(db.start_request(&lock, key(tid(1), 0, 1), now()).is_err());
    db.start_request(&lock, key(tid(1), 0, 0), now()).unwrap();
    assert!(db.start_request(&lock, key(tid(1), 0, 0), now()).is_err());
    for case in 0..9 {
        let before = records(&db);
        let mut input = reply(key(tid(1), 0, 0), 2);
        let mut candidates = vec![iid(1), iid(2)];
        match case {
            0 => candidates.pop().map(|_| ()).unwrap(),
            1 => candidates[1] = iid(1),
            2 => input.origin = ProviderOrigin::Anthropic,
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
    assert!(db.start_request(&lock, key(tid(1), 0, 1), now()).is_err());
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
                terminal: terminal(&calls.bindings[0], None)
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
                invocation_id: iid(2),
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
    assert!(
        db.finish_invocation(
            &lock,
            TerminalInput {
                invocation_id: iid(3),
                terminal: terminal(&calls.bindings[1], None)
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
            db.accept_turn(&lock, accept(sid(1), tid(1))).unwrap();
            for index in 0..=ordinal {
                db.start_request(&lock, key(tid(1), index, 0), now())
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
                finish(&db, &lock, &calls.bindings[0], None);
                finish(
                    &db,
                    &lock,
                    &calls.bindings[1],
                    Some(CepErrorCode::ExecutionFailed),
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
                        .all(|s| matches!(s.progress, OperatorInvocationState::Terminal { .. }))
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
    for case in [
        "accept_second_node",
        "continue_node",
        "stop_node",
        "change_at_max",
        "change_in_batch",
    ] {
        let (_dir, _layout, db) = store();
        let lock = db.try_lock_session(sid(1)).unwrap();
        db.accept_turn(&lock, accept(sid(1), tid(1))).unwrap();
        let calls = if matches!(case, "continue_node" | "stop_node") {
            db.start_request(&lock, key(tid(1), 0, 0), now()).unwrap();
            let calls = db
                .register_calls(&lock, reply(key(tid(1), 0, 0), 1), &[iid(1)], now())
                .unwrap();
            finish(&db, &lock, &calls.bindings[0], None);
            Some(calls)
        } else {
            None
        };
        if case.starts_with("change") {
            db.start_request(&lock, key(tid(1), 0, 0), now()).unwrap();
            let (_other, _) = running(&db, sid(2), tid(2), 1, 99);
            let tx = db.db.begin_write().unwrap();
            let mut r = load_write(&tx).unwrap();
            let high = ChangeSeq(if case == "change_in_batch" {
                u64::MAX - 1
            } else {
                u64::MAX
            });
            let mut change = r.changes.remove(&ChangeSeq(1)).unwrap();
            change.sequence = high;
            r.changes.insert(high, change);
            r.index.get_mut(&iid(99)).unwrap().last_change = high;
            r.last_change = high;
            replace_all_for_test(&tx, &r).unwrap();
            tx.commit().unwrap();
        } else {
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
        }
        let mut input = accept(sid(1), tid(3));
        input.session = AcceptSession::Existing { id: sid(1) };
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
                "accept_second_node" => db.accept_turn(&lock, input).map(|_| ()),
                "continue_node" | "stop_node" => db
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
                _ => db
                    .register_calls(&lock, reply(key(tid(1), 0, 0), 2), &[iid(1), iid(2)], now())
                    .map(|_| ()),
            },
        );
        let expected_kind = if case.starts_with("change") {
            "change"
        } else {
            "node"
        };
        assert!(
            matches!(result, Err(StoreError::SequenceExhausted { kind }) if kind == expected_kind),
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
        if case.starts_with("change") {
            for id in [iid(1), iid(2)] {
                assert!(!after.index.contains_key(&id));
                assert!(
                    !after
                        .changes
                        .values()
                        .any(|change| change.invocation_id == id)
                );
            }
        }
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
            Some(CepErrorCode::ExecutionFailed),
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
            assert_eq!(resolve_slot(slot).0.payload, ResultPayload::NotExecuted);
        }
    }
    let (_dir, _layout, db) = store();
    let (lock, calls) = running(&db, sid(1), tid(1), 2, 1);
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
    assert_eq!(detail(&db).summary.outcome, PublicTurnOutcome::Cancelled);
    let Node::Step { mut slots, .. } = node_at(&db, 2) else {
        unreachable!()
    };
    slots[0].progress = SlotProgress::Registered;
    let receipt = terminal(&slots[1].binding, None);
    slots[1].progress = SlotProgress::Terminal {
        result: receipt.result,
        evidence: receipt.evidence,
    };
    assert!(first_stopping_cause(&slots).is_err());
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
    for field in ["store_kind", "store_format_version", "last_change_seq"] {
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
    db.accept_turn(&lock, accept(sid(1), tid(1))).unwrap();
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
fn impossible_transaction_shapes_are_rejected() {
    let (_dir, layout, db) = store();
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
    let base = records(&db);
    drop(lock);
    drop(db);
    for case in [
        "premature_provider_close",
        "scheduled_provider_close",
        "exhausted_retry",
        "empty_retry",
        "nonretryable_retry",
        "request_without_sent_or_retry",
        "executing_without_completed",
        "process_exit_rewrites_retry",
    ] {
        let mut r = base.clone();
        if case == "exhausted_retry" {
            let Node::UserMessage { limits, .. } = r.nodes.get_mut(&(sid(1), NodeId(1))).unwrap()
            else {
                unreachable!()
            };
            limits.max_model_attempts = 1;
        } else {
            let Node::Step {
                progress,
                attempts,
                reply,
                ..
            } = r.nodes.get_mut(&(sid(1), NodeId(2))).unwrap()
            else {
                unreachable!()
            };
            match case {
                "premature_provider_close" | "scheduled_provider_close" => {
                    if case == "premature_provider_close" {
                        attempts[0].retry_delay_ms = None;
                    }
                    *progress = StepProgress::Closed {
                        end: StepEnd::Stop {
                            cause: StopCause::ProviderError { retryable: true },
                        },
                    };
                }
                "empty_retry" => attempts.clear(),
                "nonretryable_retry" => {
                    let AttemptProgress::Ended { end, .. } = &mut attempts[0].progress else {
                        unreachable!()
                    };
                    *end = AttemptEnd::Stopped {
                        cause: StopCause::ProviderError { retryable: false },
                    };
                }
                "request_without_sent_or_retry" => attempts[0].retry_delay_ms = None,
                "executing_without_completed" => {
                    *progress = StepProgress::Executing;
                    *reply = Some(NativeMessage::new(MessageRole::Assistant, None, vec![]));
                }
                "process_exit_rewrites_retry" => {
                    *progress = StepProgress::Closed {
                        end: StepEnd::Stop {
                            cause: StopCause::ProcessExit,
                        },
                    }
                }
                _ => unreachable!("{case}"),
            }
        }
        assert!(
            matches!(validate::records(&r), Err(StoreError::Corruption { .. })),
            "accepted {case}"
        );
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
            "open accepted {case}"
        );
    }
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
                db.rewind(&lock, NodeId(0), now()),
                Err(StoreError::Corruption { .. })
            ),
            "rewind accepted {case}"
        );
        assert!(before == records(&db), "rewind changed {case}");
    }
}
