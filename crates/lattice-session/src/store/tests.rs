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

mod attempts;
mod blobs;
mod conversation_history;
mod encoding;
mod invocations;
mod opening;
mod projections;
mod record_validation;
mod reply_validation;
mod session_settings;
mod step_progress;
mod transactions;
mod turn_acceptance;

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
        input: TurnInput::text("用户问题🌳".into()).unwrap(),
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

fn entry_nodes(entries: &[ConversationEntry]) -> Vec<u64> {
    entries.iter().map(|entry| entry.node().0).collect()
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
