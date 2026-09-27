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
            profile_snapshot_json: "{}".into(),
            policy_snapshot_json: "{}".into(),
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
    let task = io.start_execution(async move {
        assert_eq!(started.binding().invocation_id, iid(1));
        tokio::task::spawn_blocking(move || {
            let read = read_store.begin_read().unwrap();
            let state = read.durable_turn(sid(), tid(1)).unwrap();
            assert!(matches!(&state.steps[0].state, StepState::Active { active: ActiveStep::Executing { slots, .. } } if matches!(slots[0].state, SlotState::Started)));
        }).await.unwrap();
    }, Arc::default(), tokio::time::Instant::now() + std::time::Duration::from_secs(30)).await.unwrap().unwrap();
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
async fn closure_and_rewind_do_not_read_or_lock_workspace_history() {
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
    let closed = tokio::time::timeout(std::time::Duration::from_secs(3), coordinator.auto_close())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(closed, AutoCloseResult::Closed(_)));
    let before = store
        .begin_read()
        .unwrap()
        .turn_detail(sid(), tid(1))
        .unwrap();
    assert!(matches!(
        before.steps[0].state,
        StepDetailState::Closed {
            end: StepEnd::Stop {
                node: None,
                cause: StopCause::ProcessExit
            },
            ..
        }
    ));
    coordinator.rewind(NodeId(0)).await.unwrap();
    let read = store.begin_read().unwrap();
    assert_eq!(read.session(sid()).unwrap().current, NodeId(0));
    assert_eq!(read.turn_detail(sid(), tid(1)).unwrap(), before);
    assert_eq!(read.log(sid()).unwrap().nodes.len(), 2);
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
    coordinator.auto_close().await.unwrap();
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
    assert!(
        matches!(&old.steps[0].state, StepDetailState::Closed { slots, .. } if slots[0].invocation_id == iid(1))
    );
    assert_eq!(
        registered.reply.parts[0].semantic,
        MessageSemantic::Text {
            text: "说明".into()
        }
    );
}

#[tokio::test]
async fn snapshot_attempt_is_replaced_only_by_persisted_stop_or_closed_step() {
    use super::super::{
        observer::ObserverSink,
        types::{TextSegment, TurnEvent},
    };
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
    io.stop_attempt(AttemptStopped {
        key: reply.key,
        origin: reply.origin,
        usage: None,
        cause: StopCause::ProviderError { retryable: true },
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
    assert!(matches!(
        view.active.unwrap().state,
        ActiveStep::RetryReady { next_attempt: 1 }
    ));
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
        matches!(view.active.unwrap().state, ActiveStep::Executing { reply, .. } if reply == registered.reply)
    );
    assert_eq!(
        attempt.unwrap().segments,
        vec![TextSegment::Text {
            text: "说明".into()
        }]
    );
    assert_eq!(seq, 4);
    let coordinator = TurnCoordinator::new(io, Arc::new(RandomIdSource));
    coordinator.auto_close().await.unwrap();
    let (view, attempt, seq) =
        tokio::task::spawn_blocking(move || observer.snapshot(&store, sid(), Some(tid(1))))
            .await
            .unwrap()
            .unwrap();
    assert!(view.active.is_none());
    assert!(view.selected_turn.unwrap().summary.is_some());
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
                            arguments: json!({"mode":"read_file", "path":"visible.txt"}),
                        },
                    }),
                    MessagePart::semantic_only(MessageSemantic::ToolCall {
                        call: UnboundToolCall {
                            provider_call_id: Some("read-2".into()),
                            name: "observe.workspace".into(),
                            arguments: json!({"mode":"read_file", "path":"../outside.txt"}),
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
    assert_eq!(
        detail.summary.as_ref().unwrap().outcome,
        PublicTurnOutcome::Completed
    );
    assert_eq!(detail.steps.len(), 2);
    assert!(
        matches!(&detail.steps[0].state, StepDetailState::Closed { end: StepEnd::Continue { node: NodeId(2) }, slots }
        if slots.len() == 2 && matches!(slots[0].state, OperatorInvocationState::Terminal { outcome: lattice_conversation::ToolResultOutcome::Succeeded, .. })
        && matches!(slots[1].state, OperatorInvocationState::Terminal { outcome: lattice_conversation::ToolResultOutcome::Rejected, .. }))
    );
    assert!(
        matches!(&detail.steps[1].state, StepDetailState::Closed { end: StepEnd::Finish { node: NodeId(3) }, slots } if slots.is_empty())
    );
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
        matches!(&events.last().unwrap().event, super::super::types::TurnEvent::TurnCompleted { step, summary, .. } if step == &detail.steps[1] && Some(summary) == detail.summary.as_ref())
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
    assert_eq!(
        read.turn_record(tid(9)).unwrap().unwrap().original_input,
        " original \n"
    );
}

async fn stop_during_handoff(tool: bool, cancelled: bool) {
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
    io.handoff_probe = Some(Arc::new(move || {
        if calls.fetch_add(1, Ordering::SeqCst) == usize::from(tool) {
            entered_tx.lock().unwrap().take().unwrap().send(()).unwrap();
            wait.lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
        }
    }));
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
        .durable_turn(sid(), tid(1))
        .unwrap();
    if tool {
        assert!(
            matches!(&before.steps[0].state, StepState::Active { active: ActiveStep::Executing { slots, .. } } if matches!(slots[0].state, SlotState::Started) && matches!(slots[1].state, SlotState::Registered))
        );
    } else {
        assert!(matches!(
            before.steps[0].state,
            StepState::Active {
                active: ActiveStep::Requesting { .. }
            }
        ));
    }
    if cancelled {
        cancel.store(true, Ordering::Release);
    } else {
        tokio::time::sleep_until(deadline).await;
    }
    release.send(()).unwrap();
    let detail = tokio::time::timeout(Duration::from_secs(10), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let emitted = observer.events_after(0).unwrap();
    assert!(
        !emitted
            .iter()
            .any(|e| matches!(e.event, TurnEvent::ToolCallStarted { .. })),
        "stopped handoff must not dispatch a Tool"
    );
    assert_eq!(
        llm.call_count(),
        usize::from(tool),
        "stopped handoff must not open provider"
    );
    if tool {
        assert!(
            matches!(&detail.steps[0].state, StepDetailState::Closed { end: StepEnd::Stop { cause: StopCause::InvocationIndeterminate { .. }, .. }, slots } if
            matches!(slots[0].state, OperatorInvocationState::Terminal { outcome: lattice_conversation::ToolResultOutcome::Indeterminate, .. }) &&
            matches!(slots[1].state, OperatorInvocationState::Terminal { outcome: lattice_conversation::ToolResultOutcome::NotExecuted, reason: Some(OperatorReason::NotExecuted(NotExecutedReason::PrecedingInvocationIndeterminate)), .. }))
        );
    } else {
        let expected = if cancelled {
            StopCause::Cancelled
        } else {
            StopCause::Deadline
        };
        assert!(
            matches!(&detail.steps[0].state, StepDetailState::Closed { end: StepEnd::Stop { node: None, cause }, .. } if cause == &expected)
        );
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
    stop_during_handoff(false, false).await;
}
#[tokio::test]
async fn provider_handoff_wait_observes_cancel() {
    stop_during_handoff(false, true).await;
}
#[tokio::test]
async fn tool_handoff_wait_observes_deadline() {
    stop_during_handoff(true, false).await;
}
#[tokio::test]
async fn tool_handoff_wait_observes_cancel() {
    stop_during_handoff(true, true).await;
}
