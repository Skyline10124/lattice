// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use lancet_protocol::TurnId;
use lattice_conversation::{
    MessagePart, MessageRole, MessageSemantic, NativeMessage, ProviderOrigin, UnboundToolCall,
};
use lattice_session::{ProjectStoreKey, StateLayout};
use serde_json::json;

fn sid() -> SessionId {
    SessionId::parse("00000000000000000000000000000001").unwrap()
}
fn tid(n: u64) -> TurnId {
    TurnId::parse(&format!("00000000-0000-4000-8000-{n:012x}")).unwrap()
}
fn iid(n: u64) -> InvocationId {
    InvocationId::parse(&format!("00000000-0000-4000-8000-{n:012x}")).unwrap()
}
fn input() -> AcceptTurn {
    AcceptTurn {
        session: AcceptSession::New { id: sid() },
        turn_id: tid(1),
        project_store_key: ProjectStoreKey::from_canonical_path_bytes(b"/coordinator-test"),
        input: "原始输入\n".into(),
        accepted_at: chrono::Utc::now(),
        agent_selection: AgentSelection::Explicit,
        config: FrozenTurnConfig {
            agent_name: "alpha".into(),
            model: "model".into(),
            provider: "openai".into(),
            system_prompt: None,
            catalog: ToolCatalogSnapshot::new(
                "test-build".into(),
                vec![ToolCatalogEntry {
                    name: "observe.workspace".into(),
                    descriptor_identity: "a".repeat(64),
                    description: "读取".into(),
                    input_schema: json!({"type":"object"}),
                }],
            )
            .unwrap(),
            limits: TurnExecutionLimits {
                timeout_secs: 300,
                max_model_steps: 32,
                max_model_attempts: 3,
            },
        },
    }
}
fn reply(turn_id: TurnId) -> CompleteReply {
    CompleteReply {
        key: ModelAttemptKey {
            turn_id,
            model_step_ordinal: 0,
            attempt_ordinal: 0,
        },
        origin: ProviderOrigin::OpenAiCompatible,
        usage: None,
        message: NativeMessage::new(
            MessageRole::Assistant,
            Some(ProviderOrigin::OpenAiCompatible),
            vec![
                MessagePart::semantic_only(MessageSemantic::Text {
                    text: "说明".into(),
                }),
                MessagePart::semantic_only(MessageSemantic::ToolCall {
                    call: UnboundToolCall {
                        provider_call_id: Some("call-1".into()),
                        name: "observe.workspace".into(),
                        arguments: json!({}),
                    },
                }),
            ],
        ),
    }
}

#[tokio::test]
async fn started_commit_precedes_execution_and_repeated_start_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(tmp.path());
    let store = Arc::new(SessionStore::create(&layout).unwrap());
    let io = CoordinatorStoreIo::acquire(store.clone(), sid())
        .await
        .unwrap();
    assert!(matches!(
        io.accept(input()).await.unwrap(),
        AcceptedTurn::Accepted(_)
    ));
    let reply = reply(tid(1));
    io.start_request(reply.key).await.unwrap();
    let calls = io.register_calls(reply, vec![iid(1)]).await.unwrap();
    let started = io
        .start_invocation(calls.bindings[0].clone())
        .await
        .unwrap();
    let read_store = store.clone();
    let task = io
        .start_execution(
            async move {
                assert_eq!(started.binding().invocation_id, iid(1));
                tokio::task::spawn_blocking(move || {
                    let read = read_store.begin_read().unwrap();
                    let state = read.turn_detail(sid(), tid(1)).unwrap();
                    assert_eq!(state.steps[0].progress, StepProgress::Executing);
                    assert_eq!(
                        state.steps[0].slots[0].progress,
                        OperatorInvocationState::Started
                    );
                })
                .await
                .unwrap();
            },
            Arc::default(),
            tokio::time::Instant::now() + std::time::Duration::from_secs(30),
        )
        .await
        .unwrap()
        .unwrap();
    task.await.unwrap();
    assert!(
        io.start_invocation(calls.bindings[0].clone())
            .await
            .is_err()
    );
    let mut wrong = calls.bindings[0].clone();
    wrong.identity.method = "mutate.create".into();
    assert!(io.start_invocation(wrong).await.is_err());
}

#[tokio::test]
async fn orphaned_turn_rewinds_without_reading_or_locking_workspace_history() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(tmp.path());
    let input = input();
    let key = input.project_store_key;
    let history = layout
        .projects_dir()
        .join(key.to_string())
        .join("workspace-history.git");
    std::fs::create_dir_all(history.parent().unwrap()).unwrap();
    std::fs::write(&history, "损坏历史，不准访问").unwrap();
    let _workspace = lattice_store::acquire_flock(
        &layout.project_lock(&key, "workspace.lock"),
        lattice_store::LockMode::Exclusive,
        lattice_store::Blocking::NonBlocking,
    )
    .unwrap();
    let store = Arc::new(SessionStore::create(&layout).unwrap());
    let io = CoordinatorStoreIo::acquire(store.clone(), sid())
        .await
        .unwrap();
    io.accept(input).await.unwrap();
    let coordinator = TurnCoordinator::new(io, Arc::new(RandomIdSource));
    let before = store
        .begin_read()
        .unwrap()
        .turn_detail(sid(), tid(1))
        .unwrap();
    assert_eq!(before.steps[0].progress, StepProgress::Reserved);
    assert_eq!(before.summary.outcome, PublicTurnOutcome::Failed);
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        coordinator.rewind(NodeId(0)),
    )
    .await
    .unwrap()
    .unwrap();
    let read = store.begin_read().unwrap();
    assert_eq!(read.session(sid()).unwrap().current, NodeId(0));
    assert_eq!(read.turn_detail(sid(), tid(1)).unwrap(), before);
    assert_eq!(read.log(sid()).unwrap().nodes.len(), 3);
    assert_eq!(
        std::fs::read_to_string(history).unwrap(),
        "损坏历史，不准访问"
    );
}

struct Sequence(std::sync::atomic::AtomicU64);
impl IdSource for Sequence {
    fn new_invocation_id(&self) -> InvocationId {
        iid(self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst))
    }
}

#[tokio::test]
async fn collision_retries_only_registration_and_preserves_reply() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::create(&StateLayout::in_state_root(tmp.path())).unwrap());
    let io = CoordinatorStoreIo::acquire(store.clone(), sid())
        .await
        .unwrap();
    io.accept(input()).await.unwrap();
    let first = reply(tid(1));
    io.start_request(first.key).await.unwrap();
    io.register_calls(first, vec![iid(1)]).await.unwrap();
    let coordinator =
        TurnCoordinator::new(io, Arc::new(Sequence(std::sync::atomic::AtomicU64::new(1))));
    let mut second = input();
    second.session = AcceptSession::Existing { id: sid() };
    second.turn_id = tid(2);
    coordinator.io.accept(second).await.unwrap();
    let reply = reply(tid(2));
    coordinator.io.start_request(reply.key).await.unwrap();
    let registered = coordinator.register(reply).await.unwrap();
    assert_eq!(registered.bindings[0].invocation_id, iid(2));
    let read = store.begin_read().unwrap();
    let old = read.turn_detail(sid(), tid(1)).unwrap();
    assert_eq!(old.steps[0].progress, StepProgress::Executing);
    assert_eq!(old.steps[0].slots[0].invocation_id, iid(1));
    assert_eq!(
        old.steps[0].slots[0].progress,
        OperatorInvocationState::Registered
    );
    assert_eq!(
        registered.reply.parts[0].semantic,
        MessageSemantic::Text {
            text: "说明".into()
        }
    );
}

#[tokio::test]
async fn snapshot_carries_unclosed_node_and_replaces_segments_when_attempt_ends() {
    use super::super::{observer::ObserverSink, types::TurnEvent};
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::create(&StateLayout::in_state_root(tmp.path())).unwrap());
    let io = CoordinatorStoreIo::acquire(store.clone(), sid())
        .await
        .unwrap();
    io.accept(input()).await.unwrap();
    let mut reply = reply(tid(1));
    let observer = Arc::new(ObserverSink::default());
    io.start_request(reply.key).await.unwrap();
    observer.emit(TurnEvent::ModelStarted {
        key: reply.key,
        provider: reply.origin,
        model: "model".into(),
    });
    observer.emit(TurnEvent::TokenDelta {
        key: reply.key,
        content: "旧段".into(),
    });
    let read_store = store.clone();
    let sink = observer.clone();
    let (view, attempt, _) =
        tokio::task::spawn_blocking(move || sink.snapshot(&read_store, sid(), Some(tid(1))))
            .await
            .unwrap()
            .unwrap();
    assert!(matches!(view.unclosed_step, Some((NodeId(2), Node::Step {
        progress: StepProgress::Requesting, attempts, ..
    })) if matches!(attempts[0].progress, AttemptProgress::Sent)));
    assert_eq!(attempt.unwrap().key, reply.key);
    io.stop_attempt(AttemptStopped {
        key: reply.key,
        origin: reply.origin,
        usage: None,
        cause: StopCause::ProviderError { retryable: true },
        retry_delay_ms: Some(1),
        diagnostic: AttemptDiagnostic {
            provider_error: Some("retry".into()),
            ..Default::default()
        },
    })
    .await
    .unwrap();
    // 提交成功但观察事件尚未追加：已 Stopped 的旧段不能复活。
    let read_store = store.clone();
    let sink = observer.clone();
    let (view, attempt, seq) =
        tokio::task::spawn_blocking(move || sink.snapshot(&read_store, sid(), Some(tid(1))))
            .await
            .unwrap()
            .unwrap();
    assert!(
        matches!(view.unclosed_step.as_ref(), Some((NodeId(2), Node::Step {
        progress: StepProgress::Requesting, attempts, ..
    })) if attempts[0].retry_delay_ms == Some(1))
    );

    assert!(attempt.is_none());
    assert_eq!(seq, 2);
    reply.key.attempt_ordinal = 1;
    io.start_request(reply.key).await.unwrap();
    observer.emit(TurnEvent::ModelStarted {
        key: reply.key,
        provider: reply.origin,
        model: "model".into(),
    });
    observer.emit(TurnEvent::TokenDelta {
        key: reply.key,
        content: "说明".into(),
    });
    let registered = io.register_calls(reply, vec![iid(1)]).await.unwrap();
    let read_store = store.clone();
    let sink = observer.clone();
    let (view, attempt, seq) =
        tokio::task::spawn_blocking(move || sink.snapshot(&read_store, sid(), Some(tid(1))))
            .await
            .unwrap()
            .unwrap();
    assert!(
        matches!(view.unclosed_step.as_ref(), Some((NodeId(2), Node::Step {
            progress: StepProgress::Executing, reply: Some(reply), slots, ..
        })) if reply == &registered.reply && slots.len() == 1
            && slots[0].progress == SlotProgress::Registered)
    );
    assert!(attempt.is_none(), "完整回复已经提交，节点取代旧段列表");
    assert_eq!(seq, 4);
    let coordinator = TurnCoordinator::new(io, Arc::new(RandomIdSource));
    coordinator
        .io
        .close_step(CloseStep::StopExecuting {
            key: registered.key,
            stop: DispatchStop::Cancelled,
        })
        .await
        .unwrap();
    let (view, attempt, seq) =
        tokio::task::spawn_blocking(move || observer.snapshot(&store, sid(), Some(tid(1))))
            .await
            .unwrap()
            .unwrap();
    assert!(view.unclosed_step.is_none());
    assert!(view.selected_turn.unwrap().summary.progress.is_closed());
    assert!(attempt.is_none());
    assert_eq!(seq, 4);
}

#[tokio::test]
async fn live_multicall_closes_continue_then_finish_from_structured_history() {
    use super::super::{
        observer::ObserverSink,
        saga::{LiveExecution, run_coordinator_saga},
    };
    use lattice_agent::{LlmClient, ModelStepDriver};
    use lattice_llm::{ChatRequest, LlmEvent, LlmStream};
    struct Model(std::sync::Mutex<Vec<ChatRequest>>);
    #[async_trait::async_trait]
    impl LlmClient for Model {
        async fn stream_chat(
            &self,
            request: ChatRequest,
        ) -> std::result::Result<LlmStream, Box<dyn std::error::Error + Send + Sync>> {
            let mut requests = self.0.lock().unwrap();
            requests.push(request);
            let parts = if requests.len() == 1 {
                vec![
                    MessagePart::semantic_only(MessageSemantic::Text {
                        text: "先读取".into(),
                    }),
                    MessagePart::semantic_only(MessageSemantic::ToolCall {
                        call: UnboundToolCall {
                            provider_call_id: Some("read-1".into()),
                            name: "observe.workspace".into(),
                            arguments: json!({"request": {"mode":"read_file", "path":"visible.txt"}}),
                        },
                    }),
                    MessagePart::semantic_only(MessageSemantic::ToolCall {
                        call: UnboundToolCall {
                            provider_call_id: Some("read-2".into()),
                            name: "observe.workspace".into(),
                            arguments: json!({"request": {"mode":"read_file", "path":"../outside.txt"}}),
                        },
                    }),
                ]
            } else {
                vec![MessagePart::semantic_only(MessageSemantic::Text {
                    text: "已解释".into(),
                })]
            };
            Ok(Box::pin(futures::stream::iter(vec![
                LlmEvent::MessageComplete {
                    message: NativeMessage::new(
                        MessageRole::Assistant,
                        Some(ProviderOrigin::OpenAiCompatible),
                        parts,
                    ),
                },
                LlmEvent::Done {
                    finish_reason: "stop".into(),
                    usage: None,
                },
            ])))
        }
    }
    let workspace = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(state.path());
    std::fs::write(workspace.path().join("visible.txt"), "实际文件").unwrap();
    let root = workspace.path().canonicalize().unwrap();
    let key = ProjectStoreKey::derive(&root).unwrap();
    let backing = lancet_api::WorkspaceHistoryBacking::for_key(layout.clone(), key);
    lancet_api::WorkspaceHistory::create(backing.clone()).unwrap();
    let mut runtime = lancet_api::LancetRuntime::build(lancet_api::LancetRuntimeConfig {
        local_workspace_root: root.clone(),
        enabled_tools: vec!["observe.workspace".into()],
        policy: lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(root),
        capability_admission: lancet_protocol::evidence::CapabilityAdmissionPolicy {
            enabled_tools: ["observe.workspace".into()].into(),
            denied_capabilities: Default::default(),
            max_boundary: lancet_protocol::cell::Trust::Ex,
        },
        tls_roots_der: Vec::new(),
        workspace_history: backing,
    })
    .unwrap();
    let events = runtime.take_events();
    let store = Arc::new(SessionStore::create(&layout).unwrap());
    let io = CoordinatorStoreIo::acquire(store.clone(), sid())
        .await
        .unwrap();
    let mut accepted = input();
    accepted.project_store_key = key;
    accepted.config.provider = "openai_compatible".into();
    accepted.config.system_prompt = Some("冻结提示".into());
    accepted.config.catalog = ToolCatalogSnapshot::new(
        "actual-build".into(),
        runtime
            .tool_definitions()
            .iter()
            .map(|t| ToolCatalogEntry {
                name: t.name.clone(),
                description: t.description.clone(),
                descriptor_identity: t.descriptor_identity.clone(),
                input_schema: t.parameters.clone(),
            })
            .collect(),
    )
    .unwrap();
    io.accept(accepted).await.unwrap();
    let coordinator = TurnCoordinator::new(io, Arc::new(RandomIdSource));
    let llm = Arc::new(Model(std::sync::Mutex::new(Vec::new())));
    let observer = Arc::new(ObserverSink::default());
    let detail = run_coordinator_saga(
        &coordinator,
        tid(1),
        LiveExecution {
            driver: Arc::new(ModelStepDriver::new(llm.clone())),
            runtime: Arc::new(runtime),
            events,
            observer: observer.clone(),
            cancel: Arc::default(),
            shutdown: Arc::default(),
            deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(30),
            retry: Default::default(),
        },
    )
    .await
    .unwrap();
    assert_eq!(detail.summary.outcome, PublicTurnOutcome::Completed);
    assert_eq!(detail.steps.len(), 2);
    assert_eq!(detail.steps[0].node, NodeId(2));
    assert_eq!(detail.steps[0].end, StepEnd::Continue);
    let slots = &detail.steps[0].slots;
    assert_eq!(slots.len(), 2);
    assert_eq!(
        slots[0].outcome,
        lattice_conversation::ToolResultOutcome::Succeeded
    );
    assert!(
        matches!(&slots[1].reason, Some(OperatorReason::Failed { code, .. })
        if matches!(code.family(), lancet_protocol::CepErrorFamily::Validation | lancet_protocol::CepErrorFamily::Permission | lancet_protocol::CepErrorFamily::Guard))
    );
    assert_eq!(detail.steps[1].node, NodeId(3));
    assert_eq!(detail.steps[1].end, StepEnd::Finish);
    assert!(detail.steps[1].slots.is_empty());
    assert_eq!(
        store
            .begin_read()
            .unwrap()
            .turn_detail(sid(), tid(1))
            .unwrap(),
        detail
    );
    let events = observer.events_after(0).unwrap();
    assert!(
        matches!(&events.last().unwrap().event, super::super::types::TurnEvent::TurnCompleted { step, summary, .. } if step == &detail.steps[1] && summary == &detail.summary)
    );
    let requests = llm.0.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].messages.len(), 3);
    assert!(
        matches!(&requests[1].messages[2], lattice_conversation::ConversationItem::Step(step) if step.results.len() == 2)
    );
}

#[tokio::test]
async fn mixed_unknown_batch_closes_invalid_without_allocating_invocations() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::create(&StateLayout::in_state_root(tmp.path())).unwrap());
    let io = CoordinatorStoreIo::acquire(store.clone(), sid())
        .await
        .unwrap();
    io.accept(input()).await.unwrap();
    let mut reply = reply(tid(1));
    reply
        .message
        .parts
        .push(MessagePart::semantic_only(MessageSemantic::ToolCall {
            call: UnboundToolCall {
                provider_call_id: Some("unknown-2".into()),
                name: "not.enabled".into(),
                arguments: json!({}),
            },
        }));
    io.start_request(reply.key).await.unwrap();
    let ids = Arc::new(Sequence(std::sync::atomic::AtomicU64::new(100)));
    let coordinator = TurnCoordinator::new(io, ids.clone());
    assert!(matches!(
        coordinator.register(reply).await,
        Err(CoordinatorIoError::Store(StoreError::InvalidInput { .. }))
    ));
    assert_eq!(
        ids.0.load(std::sync::atomic::Ordering::SeqCst),
        100,
        "整批拒绝必须发生在身份分配之前"
    );
    let read = store.begin_read().unwrap();
    assert!(
        read.replay_audit(&input().project_store_key, sid(), tid(1))
            .unwrap()
            .calls
            .is_empty()
    );
}

#[tokio::test]
async fn first_acceptance_allocates_session_and_same_turn_reuses_it_without_configuration() {
    use super::super::{
        execute::start_turn,
        types::{RunTurnRequest, TurnAcceptance, TurnCompletion},
    };
    use lattice_llm::LlmEvent;
    let state = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(state.path());
    let store = Arc::new(SessionStore::create(&layout).unwrap());
    let api = crate::session::SessionApi::from_store(store.clone());
    let profile = project.path().join("agent.toml");
    std::fs::write(&profile, "schema_version = 1\n[agent]\nname = 'alpha'\nmodel = 'test'\n[prompt]\nsystem = 'frozen'\n[tools]\nenabled = []\n").unwrap();
    let llm = Arc::new(
        lattice_agent::testing::FakeStreamingLlmClient::builder()
            .push_events(vec![
                LlmEvent::Token {
                    content: "answer".into(),
                },
                LlmEvent::Done {
                    finish_reason: "stop".into(),
                    usage: None,
                },
            ])
            .build(),
    );
    let request = RunTurnRequest {
        input: " original \n".into(),
        session_id: None,
        agent: Some("alpha".into()),
        loaded_profile: Some(lattice_profile::load_agent_profile(&profile).unwrap()),
        cancel_flag: None,
        test_llm: Some((llm.clone(), lattice_llm::ProviderKind::OpenAiCompatible)),
        test_project_root: None,
    };
    let workspace = crate::root::LocalWorkspaceRoot::from_path(project.path()).unwrap();
    let execution = start_turn(
        api.clone(),
        layout.clone(),
        tid(9),
        workspace.clone(),
        request.clone(),
        None,
    );
    let TurnAcceptance::Accepted { turn } = execution.acceptance.await.unwrap().unwrap() else {
        panic!("not accepted")
    };
    let sid = turn.session_id;
    assert_eq!(turn.user_node, NodeId(1));
    assert!(matches!(
        execution.completion.await.unwrap().unwrap(),
        TurnCompletion::Closed(_)
    ));
    let mut retry = request;
    retry.loaded_profile = None;
    retry.agent = None;
    retry.test_llm = None;
    let execution = start_turn(api, layout, tid(9), workspace, retry, None);
    let TurnAcceptance::Existing { turn } = execution.acceptance.await.unwrap().unwrap() else {
        panic!("not found")
    };
    assert_eq!(turn.session_id, sid);
    assert!(matches!(
        execution.completion.await.unwrap().unwrap(),
        TurnCompletion::Closed(_)
    ));
    assert_eq!(llm.call_count(), 1);
    let read = store.begin_read().unwrap();
    assert_eq!(read.list(None, 100).unwrap().items.len(), 1);
    assert_eq!(read.turn_input(sid, tid(9)).unwrap(), " original \n");
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StopBoundary {
    ProviderHandoff,
    ToolHandoff,
    CompletedBatch,
}

async fn stop_at_execution_boundary(boundary: StopBoundary, cancelled: bool) {
    let tool = boundary != StopBoundary::ProviderHandoff;
    let completed_batch = boundary == StopBoundary::CompletedBatch;
    use super::super::{
        observer::ObserverSink,
        saga::{LiveExecution, run_coordinator_saga},
        types::TurnEvent,
    };
    use lattice_agent::{ModelStepDriver, testing::FakeStreamingLlmClient};
    use lattice_llm::LlmEvent;
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use std::time::Duration;
    let workspace = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(state.path());
    let root = workspace.path().canonicalize().unwrap();
    let project = ProjectStoreKey::derive(&root).unwrap();
    let backing = lancet_api::WorkspaceHistoryBacking::for_key(layout.clone(), project);
    lancet_api::WorkspaceHistory::create(backing.clone()).unwrap();
    let mut runtime = lancet_api::LancetRuntime::build(lancet_api::LancetRuntimeConfig {
        local_workspace_root: root.clone(),
        enabled_tools: vec!["observe.workspace".into()],
        policy: lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(root),
        capability_admission: lancet_protocol::evidence::CapabilityAdmissionPolicy {
            enabled_tools: ["observe.workspace".into()].into(),
            denied_capabilities: Default::default(),
            max_boundary: lancet_protocol::cell::Trust::Ex,
        },
        tls_roots_der: Vec::new(),
        workspace_history: backing,
    })
    .unwrap();
    let events = runtime.take_events();
    let store = Arc::new(SessionStore::create(&layout).unwrap());
    let mut io = CoordinatorStoreIo::acquire(store.clone(), sid())
        .await
        .unwrap();
    let mut accepted = input();
    accepted.project_store_key = project;
    accepted.config.catalog = ToolCatalogSnapshot::new(
        "actual".into(),
        runtime
            .tool_definitions()
            .iter()
            .map(|t| ToolCatalogEntry {
                name: t.name.clone(),
                description: t.description.clone(),
                descriptor_identity: t.descriptor_identity.clone(),
                input_schema: t.parameters.clone(),
            })
            .collect(),
    )
    .unwrap();
    io.accept(accepted).await.unwrap();
    let mut message = reply(tid(1)).message;
    message.parts.push(message.parts[1].clone());
    if let MessageSemantic::ToolCall { call } = &mut message.parts[2].semantic {
        call.provider_call_id = Some("call-2".into());
    }
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(vec![
                LlmEvent::MessageComplete { message },
                LlmEvent::Done {
                    finish_reason: "tool_calls".into(),
                    usage: None,
                },
            ])
            .build(),
    );
    let (entered_tx, entered) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let calls = AtomicUsize::new(0);
    let entered_tx = Mutex::new(Some(entered_tx));
    let wait = Mutex::new(wait);
    let probe: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        if calls.fetch_add(1, Ordering::SeqCst) == usize::from(tool) {
            entered_tx.lock().unwrap().take().unwrap().send(()).unwrap();
            wait.lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
        }
    });
    if completed_batch {
        io.finish_probe = Some(probe);
        // 若 saga 仍先提交 Continue，这次提交结果未知会遗留 Reserved，无法读回实际停止原因。
        io.close_fault = Some(TestCommitFault::UnknownAfter);
    } else {
        io.handoff_probe = Some(probe);
    }
    let coordinator = TurnCoordinator::new(io, Arc::new(RandomIdSource));
    let cancel = Arc::new(AtomicBool::new(false));
    let observer = Arc::new(ObserverSink::default());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let live = LiveExecution {
        driver: Arc::new(ModelStepDriver::new(llm.clone())),
        runtime: Arc::new(runtime),
        events,
        observer: observer.clone(),
        cancel: cancel.clone(),
        shutdown: Arc::default(),
        deadline,
        retry: Default::default(),
    };
    let running =
        tokio::spawn(async move { run_coordinator_saga(&coordinator, tid(1), live).await });
    tokio::time::timeout(Duration::from_secs(10), entered)
        .await
        .unwrap()
        .unwrap();
    let before = store
        .begin_read()
        .unwrap()
        .turn_detail(sid(), tid(1))
        .unwrap();
    if completed_batch {
        assert_eq!(before.steps[0].progress, StepProgress::Executing);
        assert!(
            before.steps[0]
                .slots
                .iter()
                .all(|s| matches!(s.progress, OperatorInvocationState::Terminal { .. }))
        );
    } else if tool {
        assert_eq!(before.steps[0].progress, StepProgress::Executing);
        assert_eq!(
            before.steps[0].slots[0].progress,
            OperatorInvocationState::Started
        );
        assert_eq!(
            before.steps[0].slots[1].progress,
            OperatorInvocationState::Registered
        );
    } else {
        assert_eq!(before.steps[0].progress, StepProgress::Requesting);
        assert!(matches!(
            before.steps[0].attempts[0].progress,
            AttemptProgressDetail::Sent
        ));
    }
    if cancelled {
        cancel.store(true, Ordering::Release);
    } else {
        tokio::time::sleep_until(deadline).await;
    }
    release.send(()).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), running)
        .await
        .unwrap()
        .unwrap();
    let detail = if completed_batch {
        assert!(matches!(
            result,
            Err(CoordinatorIoError::Store(StoreError::CommitFailed {
                kind: CommitFailureKind::OutcomeUnknown,
                ..
            }))
        ));
        store
            .begin_read()
            .unwrap()
            .turn_detail(sid(), tid(1))
            .unwrap()
    } else {
        result.unwrap()
    };
    let emitted = observer.events_after(0).unwrap();
    let starts = emitted
        .iter()
        .filter(|e| matches!(e.event, TurnEvent::ToolCallStarted { .. }))
        .count();
    assert_eq!(starts, if completed_batch { 2 } else { 0 });
    assert_eq!(
        llm.call_count(),
        usize::from(tool),
        "stopped handoff must not open provider"
    );
    let expected = if cancelled {
        StopCause::Cancelled
    } else {
        StopCause::Deadline
    };
    if completed_batch {
        assert_eq!(detail.steps.len(), 2);
        assert_eq!(detail.steps[0].end, StepEnd::Continue);
        assert_eq!(detail.steps[0].slots, before.steps[0].slots);
        assert!(detail.steps[0].progress.is_closed());
        let last = &detail.steps[1];
        assert_eq!(last.end, StepEnd::Stop { cause: expected });
        assert!(last.attempts.is_empty());
        assert!(last.slots.is_empty());
        assert!(last.progress.is_closed());
        assert_eq!(detail.summary.progress, last.progress);
        assert_eq!(
            detail.summary.outcome,
            if cancelled {
                PublicTurnOutcome::Cancelled
            } else {
                PublicTurnOutcome::Failed
            }
        );
        assert_eq!(
            store.begin_read().unwrap().session(sid()).unwrap().current,
            last.node
        );
        // 只有提交回执丢失，重开不做补写也必须读出同一结局。
        drop(store);
        let reopened = SessionStore::open(&layout).unwrap();
        assert_eq!(
            reopened
                .begin_read()
                .unwrap()
                .turn_detail(sid(), tid(1))
                .unwrap(),
            detail
        );
        return;
    }
    // 停止发生在交接之前：已 Started 的槽位仍保留 NoResult 的结论。
    assert_eq!(
        detail.steps[0].end,
        StepEnd::Stop {
            cause: expected.clone()
        }
    );
    assert!(detail.steps[0].progress.is_closed());
    if tool {
        let slots = &detail.steps[0].slots;
        assert_eq!(slots[0].progress, OperatorInvocationState::Started);
        assert_eq!(
            slots[0].outcome,
            lattice_conversation::ToolResultOutcome::Indeterminate
        );
        assert_eq!(
            slots[0].reason,
            Some(OperatorReason::Indeterminate(IndeterminateCause::NoResult))
        );
        assert_eq!(slots[1].progress, OperatorInvocationState::Registered);
        assert_eq!(
            slots[1].outcome,
            lattice_conversation::ToolResultOutcome::NotExecuted
        );
    } else {
        assert!(
            matches!(&detail.steps[0].attempts[0].end, AttemptEndDetail::Stopped { cause, .. } if cause == &expected)
        );
    }
    assert_eq!(
        store
            .begin_read()
            .unwrap()
            .turn_detail(sid(), tid(1))
            .unwrap(),
        detail
    );
}

#[tokio::test]
async fn provider_handoff_wait_observes_deadline() {
    stop_at_execution_boundary(StopBoundary::ProviderHandoff, false).await;
}
#[tokio::test]
async fn provider_handoff_wait_observes_cancel() {
    stop_at_execution_boundary(StopBoundary::ProviderHandoff, true).await;
}
#[tokio::test]
async fn tool_handoff_wait_observes_deadline() {
    stop_at_execution_boundary(StopBoundary::ToolHandoff, false).await;
}
#[tokio::test]
async fn tool_handoff_wait_observes_cancel() {
    stop_at_execution_boundary(StopBoundary::ToolHandoff, true).await;
}

#[tokio::test]
async fn completed_batch_cancel_survives_unknown_close_commit() {
    stop_at_execution_boundary(StopBoundary::CompletedBatch, true).await;
}

#[tokio::test]
async fn completed_batch_deadline_survives_unknown_close_commit() {
    stop_at_execution_boundary(StopBoundary::CompletedBatch, false).await;
}
