// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use lattice_conversation::{
    MessagePart, MessageRole, MessageSemantic, NativeMessage, Protocol, TurnId, UnboundToolCall,
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
    let config = FrozenTurnConfig {
        effort: None,
        agent_name: crate::AgentName::parse("alpha").unwrap(),
        model: crate::ModelId::parse("model").unwrap(),
        provider: crate::ProviderId::parse("openai").unwrap(),
        system_prompt: None,
        catalog: ToolCatalogSnapshot::new(vec![ToolCatalogEntry {
            name: "observe__workspace".into(),
            tool_identity: "test:observe__workspace@1".into(),
            description: "读取".into(),
            input_schema: json!({"type":"object"}),
        }])
        .unwrap(),
        limits: TurnExecutionLimits {
            timeout_secs: crate::TurnTimeout::default(),
            max_model_steps: 32,
            max_model_attempts: 3,
        },
    };
    AcceptTurn {
        profile_copy: lattice_session::store::SessionProfileCopy::for_test(&config),
        expected_profile_digest: None,
        session: AcceptSession::New { id: sid() },
        turn_id: tid(1),
        project_store_key: ProjectStoreKey::from_canonical_path_bytes(b"/coordinator-test"),
        input: "原始输入\n".into(),
        accepted_at: chrono::Utc::now(),
        agent_selection: AgentSelection::Explicit,
        config,
    }
}
fn origin() -> AttemptOrigin {
    AttemptOrigin {
        protocol: AttemptProtocol::ChatCompletions,
        provider_id: crate::ProviderId::parse("openai").unwrap(),
    }
}
fn reply(turn_id: TurnId) -> CompleteReply {
    CompleteReply {
        usage_kind: lattice_conversation::UsageCompleteness::Partial,
        cost: None,
        key: ModelAttemptKey {
            turn_id,
            model_step_ordinal: 0,
            attempt_ordinal: 0,
        },
        origin: origin(),
        usage: None,
        message: NativeMessage::new(
            MessageRole::Assistant,
            Some(lattice_conversation::ReplayOrigin {
                protocol: Protocol::ChatCompletions,
                provider_id: crate::ProviderId::parse("openai").unwrap(),
            }),
            vec![
                MessagePart::semantic_only(MessageSemantic::Text {
                    text: "说明".into(),
                }),
                MessagePart::semantic_only(MessageSemantic::ToolCall {
                    call: UnboundToolCall {
                        provider_call_id: Some("call-1".into()),
                        name: "observe__workspace".into(),
                        arguments: json!({}),
                    },
                }),
            ],
        ),
    }
}

type CallbackFn = dyn Fn(
        StartedInvocation,
        &str,
        serde_json::Value,
        Arc<std::sync::atomic::AtomicBool>,
        tokio::time::Instant,
    ) -> crate::ToolOutcome
    + Send
    + Sync;

struct CallbackTools {
    specs: Vec<crate::ToolSpec>,
    run: Box<CallbackFn>,
}

impl CallbackTools {
    fn single(
        name: &str,
        run: impl Fn(
            StartedInvocation,
            &str,
            serde_json::Value,
            Arc<std::sync::atomic::AtomicBool>,
            tokio::time::Instant,
        ) -> crate::ToolOutcome
        + Send
        + Sync
        + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            specs: vec![crate::ToolSpec {
                name: name.to_owned(),
                description: "测试工具".into(),
                input_schema: json!({"type":"object"}),
                tool_identity: format!("test:{name}@1"),
            }],
            run: Box::new(run),
        })
    }
}

#[async_trait::async_trait]
impl crate::TurnTools for CallbackTools {
    fn specs(&self) -> &[crate::ToolSpec] {
        &self.specs
    }

    async fn execute(
        &self,
        started: StartedInvocation,
        name: &str,
        arguments: serde_json::Value,
        cancel: Arc<std::sync::atomic::AtomicBool>,
        deadline: tokio::time::Instant,
    ) -> crate::ToolOutcome {
        (self.run)(started, name, arguments, cancel, deadline)
    }
}

#[async_trait::async_trait]
impl crate::ToolExecutor for CallbackTools {
    async fn open_turn(
        &self,
        _session_id: SessionId,
        _workspace: &crate::LocalWorkspaceRoot,
    ) -> crate::session::Result<Box<dyn crate::TurnTools>> {
        panic!("测试若经 start_turn 注入请使用 SharedExecutor 包装")
    }
}

struct SharedExecutor(Arc<dyn crate::TurnTools>);

#[async_trait::async_trait]
impl crate::ToolExecutor for SharedExecutor {
    async fn open_turn(
        &self,
        _session_id: SessionId,
        _workspace: &crate::LocalWorkspaceRoot,
    ) -> crate::session::Result<Box<dyn crate::TurnTools>> {
        struct Delegate(Arc<dyn crate::TurnTools>);
        #[async_trait::async_trait]
        impl crate::TurnTools for Delegate {
            fn specs(&self) -> &[crate::ToolSpec] {
                self.0.specs()
            }
            async fn execute(
                &self,
                started: StartedInvocation,
                name: &str,
                arguments: serde_json::Value,
                cancel: Arc<std::sync::atomic::AtomicBool>,
                deadline: tokio::time::Instant,
            ) -> crate::ToolOutcome {
                self.0
                    .execute(started, name, arguments, cancel, deadline)
                    .await
            }
        }
        Ok(Box::new(Delegate(self.0.clone())))
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
    io.start_request(reply.key, reply.origin.clone())
        .await
        .unwrap();
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
                    assert_eq!(state.steps[0].slots[0].progress, SlotState::Started);
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
    wrong.identity.method = "mutate__create".into();
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
    io.start_request(first.key, first.origin.clone())
        .await
        .unwrap();
    io.register_calls(first, vec![iid(1)]).await.unwrap();
    let coordinator =
        TurnCoordinator::new(io, Arc::new(Sequence(std::sync::atomic::AtomicU64::new(1))));
    let mut second = input();
    second.session = AcceptSession::Existing { id: sid() };
    second.turn_id = tid(2);
    second.expected_profile_digest = Some(input().profile_copy.digest().unwrap());
    coordinator.io.accept(second).await.unwrap();
    let reply = reply(tid(2));
    coordinator
        .io
        .start_request(reply.key, reply.origin.clone())
        .await
        .unwrap();
    let registered = coordinator.register(reply).await.unwrap();
    assert_eq!(registered.bindings[0].invocation_id, iid(2));
    let read = store.begin_read().unwrap();
    let old = read.turn_detail(sid(), tid(1)).unwrap();
    assert_eq!(old.steps[0].progress, StepProgress::Executing);
    assert_eq!(old.steps[0].slots[0].invocation_id, iid(1));
    assert_eq!(old.steps[0].slots[0].progress, SlotState::Registered);
    assert_eq!(
        registered.reply.parts[0].semantic,
        MessageSemantic::Text {
            text: "说明".into()
        }
    );
}

#[tokio::test]
async fn live_multicall_closes_continue_then_finish_from_structured_history() {
    use super::super::{
        observer::ObserverSink,
        saga::{LiveExecution, run_coordinator_saga},
    };
    use lattice_agent::{LlmClient, ModelStepDriver};
    use lattice_conversation::ToolError;
    use lattice_llm::testing::ScriptEvent;
    use lattice_llm::{ChatRequest, LlmStream};
    struct Model(std::sync::Mutex<Vec<ChatRequest>>);
    #[async_trait::async_trait]
    impl LlmClient for Model {
        async fn stream_chat(
            &self,
            request: ChatRequest,
            control: lattice_llm::StreamControl,
        ) -> std::result::Result<LlmStream, Box<dyn std::error::Error + Send + Sync>> {
            let mut requests = self.0.lock().unwrap();
            let source = lattice_conversation::ReplayOrigin {
                protocol: Protocol::ChatCompletions,
                provider_id: request.target.model.provider().clone(),
            };
            requests.push(request);
            let parts = if requests.len() == 1 {
                vec![
                    MessagePart::semantic_only(MessageSemantic::Text {
                        text: "先读取".into(),
                    }),
                    MessagePart::semantic_only(MessageSemantic::ToolCall {
                        call: UnboundToolCall {
                            provider_call_id: Some("read-1".into()),
                            name: "observe__workspace".into(),
                            arguments: json!({"path":"visible.txt"}),
                        },
                    }),
                    MessagePart::semantic_only(MessageSemantic::ToolCall {
                        call: UnboundToolCall {
                            provider_call_id: Some("read-2".into()),
                            name: "observe__workspace".into(),
                            arguments: json!({"path":"../outside.txt"}),
                        },
                    }),
                ]
            } else {
                vec![MessagePart::semantic_only(MessageSemantic::Text {
                    text: "已解释".into(),
                })]
            };
            let mut events = lattice_llm::testing::parts_events(parts);
            events.push(ScriptEvent::Done {
                finish_reason: "stop".into(),
                usage: None,
            });
            Ok(lattice_llm::testing::script_stream(source, events, control))
        }
    }
    let workspace = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(state.path());
    let root = workspace.path().canonicalize().unwrap();
    let key = ProjectStoreKey::derive(&root).unwrap();
    let tools = CallbackTools::single("observe__workspace", |_, _, args, _, _| {
        if args.get("path").and_then(|v| v.as_str()) == Some("visible.txt") {
            crate::ToolOutcome::Succeeded(json!({"content": "实际文件"}))
        } else {
            crate::ToolOutcome::Failed(ToolError {
                code: "permission_denied".into(),
                message: "路径越界".into(),
            })
        }
    });
    let store = Arc::new(SessionStore::create(&layout).unwrap());
    let io = CoordinatorStoreIo::acquire(store.clone(), sid())
        .await
        .unwrap();
    let mut accepted = input();
    accepted.project_store_key = key;
    accepted.config.provider = crate::ProviderId::parse("openai-compatible").unwrap();
    accepted.config.system_prompt = Some("冻结提示".into());
    accepted.config.catalog = ToolCatalogSnapshot::new(
        tools
            .specs
            .iter()
            .map(|t| ToolCatalogEntry {
                name: t.name.clone(),
                description: t.description.clone(),
                tool_identity: t.tool_identity.clone(),
                input_schema: t.input_schema.clone(),
            })
            .collect(),
    )
    .unwrap();
    accepted.config.effort = Some(lattice_conversation::Effort::High);
    accepted.profile_copy = SessionProfileCopy::for_test(&accepted.config);
    io.accept(accepted).await.unwrap();
    let coordinator = TurnCoordinator::new(io, Arc::new(RandomIdSource));
    let llm = Arc::new(Model(std::sync::Mutex::new(Vec::new())));
    let observer = Arc::new(ObserverSink::default());
    let detail = run_coordinator_saga(
        &coordinator,
        tid(1),
        LiveExecution {
            model: crate::turn::saga::ModelExecution::Injected {
                driver: Arc::new(ModelStepDriver::new(llm.clone())),
                origin: AttemptOrigin {
                    provider_id: crate::ProviderId::parse("openai-compatible").unwrap(),
                    protocol: AttemptProtocol::ChatCompletions,
                },
            },
            tools,
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
    assert_eq!(
        slots[1].reason,
        Some(SlotReason::Failed {
            code: "permission_denied".into(),
        })
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
    let events = observer.events();
    assert!(
        matches!(&events.last().unwrap(), super::super::types::TurnEvent::TurnCompleted { step, summary, .. } if step == &detail.steps[1] && summary == &detail.summary)
    );
    let requests = llm.0.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|request| request.target.effort == Some(lattice_conversation::Effort::High))
    );
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
    io.start_request(reply.key, reply.origin.clone())
        .await
        .unwrap();
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
        read.turn_detail(sid(), tid(1)).unwrap().steps[0]
            .slots
            .is_empty()
    );
}

#[tokio::test]
async fn first_acceptance_reopens_with_frozen_facts_and_used_turn_conflicts() {
    use super::super::{
        execute::start_turn,
        types::{RunTurnRequest, TurnAcceptance},
    };
    use lattice_llm::testing::ScriptEvent;
    let state = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(state.path());
    let store = Arc::new(SessionStore::create(&layout).unwrap());
    let api = crate::session::SessionApi::from_store(store.clone());
    let paths = lattice_profile::DiscoveryPaths {
        project_root: project.path().to_path_buf(),
        global_agents_dir: state.path().join("global-agents"),
    };
    let profile = paths.project_agents_dir().join("alpha/agent.toml");
    std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
    std::fs::write(
        &profile,
        "schema_version = 1\n[agent]\nname = 'alpha'\nmodel = 'local/test'\n[prompt]\nsystem = 'frozen'\n",
    )
    .unwrap();
    let llm = Arc::new(
        lattice_agent::testing::FakeStreamingLlmClient::builder()
            .push_events(vec![
                ScriptEvent::Text {
                    content: "answer".into(),
                },
                ScriptEvent::Done {
                    finish_reason: "stop".into(),
                    usage: None,
                },
            ])
            .build(),
    );
    let alpha = crate::AgentName::parse("alpha").unwrap();
    let loaded_profile =
        lattice_profile::load_agent_profile(&alpha, &paths, crate::local::ipc::uid()).unwrap();
    let request = RunTurnRequest {
        model: None,
        effort: None,
        input: " original \n".into(),
        session_id: None,
        agent: Some(alpha),
        loaded_profile: Some(loaded_profile),
        cancel_flag: None,
        tools: None,
        test_llm: Some((
            llm.clone(),
            lattice_session::store::AttemptProtocol::ChatCompletions,
            vec![],
        )),
    };
    let workspace = crate::root::LocalWorkspaceRoot::from_path(project.path()).unwrap();
    let execution = start_turn(api.clone(), tid(9), workspace.clone(), request.clone());
    let TurnAcceptance::Accepted { turn } = execution.acceptance.await.unwrap().unwrap() else {
        panic!("not accepted")
    };
    let sid = turn.session_id;
    assert_eq!(turn.user_node, NodeId(1));
    execution.completion.await.unwrap().unwrap();
    let mut retry = request;
    retry.loaded_profile = None;
    retry.agent = None;
    retry.test_llm = None;
    let execution = start_turn(api, tid(9), workspace, retry);
    assert!(matches!(
        execution.acceptance.await.unwrap(),
        Err(crate::ApiError::Conflict(
            crate::session::RequestConflict::TurnIdUsed
        ))
    ));
    assert!(execution.completion.await.unwrap().is_err());
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
    use lattice_llm::testing::ScriptEvent;
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
    let tools = CallbackTools::single("observe__workspace", |_, _, _, _, _| {
        crate::ToolOutcome::Succeeded(json!({"ok": true}))
    });
    let store = Arc::new(SessionStore::create(&layout).unwrap());
    let mut io = CoordinatorStoreIo::acquire(store.clone(), sid())
        .await
        .unwrap();
    let mut accepted = input();
    accepted.project_store_key = project;
    accepted.config.catalog = ToolCatalogSnapshot::new(
        tools
            .specs
            .iter()
            .map(|t| ToolCatalogEntry {
                name: t.name.clone(),
                description: t.description.clone(),
                tool_identity: t.tool_identity.clone(),
                input_schema: t.input_schema.clone(),
            })
            .collect(),
    )
    .unwrap();
    accepted.config.effort = Some(lattice_conversation::Effort::High);
    accepted.profile_copy = SessionProfileCopy::for_test(&accepted.config);
    io.accept(accepted).await.unwrap();
    let mut message = reply(tid(1)).message;
    message.parts.push(message.parts[1].clone());
    if let MessageSemantic::ToolCall { call } = &mut message.parts[2].semantic {
        call.provider_call_id = Some("call-2".into());
    }
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(
                lattice_llm::testing::parts_events(message.parts)
                    .into_iter()
                    .chain([ScriptEvent::Done {
                        finish_reason: "tool_calls".into(),
                        usage: None,
                    }])
                    .collect(),
            )
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
        model: crate::turn::saga::ModelExecution::Injected {
            driver: Arc::new(ModelStepDriver::new(llm.clone())),
            origin: AttemptOrigin {
                provider_id: crate::ProviderId::parse("openai").unwrap(),
                protocol: AttemptProtocol::ChatCompletions,
            },
        },
        tools,
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
                .all(|s| matches!(s.progress, SlotState::Terminal { .. }))
        );
    } else if tool {
        assert_eq!(before.steps[0].progress, StepProgress::Executing);
        assert_eq!(before.steps[0].slots[0].progress, SlotState::Started);
        assert_eq!(before.steps[0].slots[1].progress, SlotState::Registered);
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
    let emitted = observer.events();
    let starts = emitted
        .iter()
        .filter(|e| matches!(e, TurnEvent::ToolCallStarted { .. }))
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
        assert_eq!(slots[0].progress, SlotState::Started);
        assert_eq!(
            slots[0].outcome,
            lattice_conversation::ToolResultOutcome::Indeterminate
        );
        assert_eq!(
            slots[0].reason,
            Some(SlotReason::Indeterminate(IndeterminateCause::NoResult))
        );
        assert_eq!(slots[1].progress, SlotState::Registered);
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

fn tool_call_events(
    calls: &[(&str, &str, serde_json::Value)],
) -> Vec<lattice_llm::testing::ScriptEvent> {
    let parts = calls
        .iter()
        .map(|(id, name, args)| {
            MessagePart::semantic_only(MessageSemantic::ToolCall {
                call: UnboundToolCall {
                    provider_call_id: Some((*id).to_owned()),
                    name: (*name).to_owned(),
                    arguments: args.clone(),
                },
            })
        })
        .collect();
    lattice_llm::testing::parts_events(parts)
        .into_iter()
        .chain([lattice_llm::testing::ScriptEvent::Done {
            finish_reason: "tool_calls".into(),
            usage: None,
        }])
        .collect()
}

#[tokio::test]
async fn unlisted_tool_name_closes_step_with_invalid_batch() {
    use super::super::test_support::setup_with_events;
    let fixture = setup_with_events(
        tool_call_events(&[("call-1", "unlisted_tool", json!({}))]),
        &[],
    );
    let (detail, _) = fixture.run(fixture.request.clone()).await;
    assert_eq!(detail.summary.outcome, PublicTurnOutcome::Failed);
    assert_eq!(
        detail.summary.notice.as_ref().unwrap().text,
        "invalid tool call batch"
    );
    assert_eq!(
        detail.steps[0].end,
        StepEnd::Stop {
            cause: StopCause::InvalidBatch
        }
    );
    assert!(detail.steps[0].slots.is_empty());
}

#[tokio::test]
async fn executor_deadline_after_started_leaves_slot_started_no_result_and_skips_rest() {
    use super::super::test_support::{happy_events, setup_fixture};
    use lattice_agent::testing::FakeStreamingLlmClient;
    use lattice_conversation::ToolResultOutcome;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(tool_call_events(&[
                ("call-1", "work", json!({"seq": 1})),
                ("call-2", "work", json!({"seq": 2})),
            ]))
            .push_events(happy_events())
            .build(),
    );
    let fixture = setup_fixture(llm.clone(), &["work"]);
    let executed = Arc::new(AtomicUsize::new(0));
    let executed_clone = executed.clone();
    let store_for_check = fixture.store.clone();
    let tools = CallbackTools::single("work", move |started, _, _, _, _| {
        executed_clone.fetch_add(1, Ordering::SeqCst);
        let read = store_for_check.begin_read().unwrap();
        let session_id = started.binding().identity.session_id;
        let turn_id = started.binding().identity.turn_id;
        let snapshot = read.turn_detail(session_id, turn_id).unwrap();
        assert_eq!(snapshot.steps[0].slots[0].progress, SlotState::Started);
        assert_eq!(snapshot.steps[0].slots[1].progress, SlotState::Registered);
        crate::ToolOutcome::Deadline
    });
    let mut request = fixture.request.clone();
    request.tools = Some(Arc::new(SharedExecutor(tools)));

    let (detail, _) = fixture.run(request).await;
    assert_eq!(executed.load(Ordering::SeqCst), 1, "后续槽位不得再分发执行");
    assert_eq!(llm.call_count(), 1, "Deadline 关闭后不得发起下一步模型请求");
    assert_eq!(detail.summary.outcome, PublicTurnOutcome::Failed);
    assert_eq!(
        detail.summary.notice.as_ref().unwrap().text,
        "turn deadline exceeded"
    );
    assert_eq!(
        detail.steps[0].end,
        StepEnd::Stop {
            cause: StopCause::Deadline
        }
    );
    let slots = &detail.steps[0].slots;
    assert_eq!(slots.len(), 2);
    assert_eq!(slots[0].progress, SlotState::Started);
    assert_eq!(slots[0].outcome, ToolResultOutcome::Indeterminate);
    assert_eq!(
        slots[0].reason,
        Some(SlotReason::Indeterminate(IndeterminateCause::NoResult))
    );
    assert_eq!(slots[1].progress, SlotState::Registered);
    assert_eq!(slots[1].outcome, ToolResultOutcome::NotExecuted);
    assert_eq!(slots[1].reason, None);
}

#[tokio::test]
async fn executor_cancelled_and_indeterminate_outcomes_stop_batch_and_record_terminal_slots() {
    use super::super::test_support::{happy_events, setup_fixture};
    use lattice_agent::testing::FakeStreamingLlmClient;
    use lattice_conversation::ToolResultOutcome;

    for (outcome, expected_turn, expected_reason) in [
        (
            crate::ToolOutcome::Cancelled,
            PublicTurnOutcome::Cancelled,
            SlotReason::Indeterminate(IndeterminateCause::Cancelled),
        ),
        (
            crate::ToolOutcome::Indeterminate,
            PublicTurnOutcome::Failed,
            SlotReason::Indeterminate(IndeterminateCause::Reported),
        ),
    ] {
        let llm = Arc::new(
            FakeStreamingLlmClient::builder()
                .push_events(tool_call_events(&[
                    ("call-1", "work", json!({})),
                    ("call-2", "work", json!({})),
                ]))
                .push_events(happy_events())
                .build(),
        );
        let fixture = setup_fixture(llm.clone(), &["work"]);
        let tools = CallbackTools::single("work", move |_, _, _, _, _| outcome.clone());
        let mut request = fixture.request.clone();
        request.tools = Some(Arc::new(SharedExecutor(tools)));

        let (detail, _) = fixture.run(request).await;
        assert_eq!(llm.call_count(), 1);
        assert_eq!(detail.summary.outcome, expected_turn);
        let slots = &detail.steps[0].slots;
        assert_eq!(slots.len(), 2);
        assert!(matches!(slots[0].progress, SlotState::Terminal { .. }));
        assert_eq!(slots[0].outcome, ToolResultOutcome::Indeterminate);
        assert_eq!(slots[0].reason, Some(expected_reason));
        assert_eq!(slots[1].progress, SlotState::Registered);
        assert_eq!(slots[1].outcome, ToolResultOutcome::NotExecuted);
        if expected_turn == PublicTurnOutcome::Cancelled {
            assert_eq!(
                detail.steps[0].end,
                StepEnd::Stop {
                    cause: StopCause::Cancelled
                }
            );
        } else {
            assert_eq!(
                detail.steps[0].end,
                StepEnd::Stop {
                    cause: StopCause::InvocationIndeterminate {
                        invocation_id: slots[0].invocation_id
                    }
                }
            );
        }
    }
}

#[tokio::test]
async fn executor_oversized_output_stores_digest_only_and_continues_next_step() {
    use super::super::test_support::{happy_events, setup_fixture};
    use lattice_agent::testing::FakeStreamingLlmClient;
    use lattice_conversation::ToolResultOutcome;

    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(tool_call_events(&[("call-1", "big", json!({}))]))
            .push_events(happy_events())
            .build(),
    );
    let fixture = setup_fixture(llm.clone(), &["big"]);
    let tools = CallbackTools::single("big", |_, _, _, _, _| {
        crate::ToolOutcome::Succeeded(json!({
            "blob": "a".repeat(MAX_TOOL_CONTENT_BYTES + 128),
        }))
    });
    let mut request = fixture.request.clone();
    request.tools = Some(Arc::new(SharedExecutor(tools)));

    let (detail, _) = fixture.run(request).await;
    assert_eq!(detail.summary.outcome, PublicTurnOutcome::Completed);
    assert_eq!(llm.call_count(), 2);
    assert_eq!(detail.steps.len(), 2);
    assert_eq!(detail.steps[0].end, StepEnd::Continue);
    let slot = &detail.steps[0].slots[0];
    assert!(matches!(slot.progress, SlotState::Terminal { .. }));
    assert_eq!(slot.outcome, ToolResultOutcome::Succeeded);
    assert!(matches!(
        &slot.material,
        MaterialSummary::OutputTooLarge { byte_length, .. }
            if *byte_length > MAX_TOOL_CONTENT_BYTES as u64
    ));
}

#[cfg(feature = "test-fake-tools")]
mod fake_tools_tests {
    use super::*;
    use crate::turn::test_support::{happy_events, setup_fixture};
    use crate::turn::tools::fake::{FakeToolExecutor, TEST_TOOLS_ENV};
    use lattice_agent::LlmClient;
    use lattice_agent::testing::FakeStreamingLlmClient;
    use lattice_conversation::{ResultPayload, ToolError, ToolResultOutcome};
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn unset_test_tools_env_defaults_to_no_tools_with_empty_catalog_and_model_request() {
        assert!(
            std::env::var_os(TEST_TOOLS_ENV).is_none(),
            "默认单元测试环境不应预设 LATTICE_TEST_TOOLS"
        );
        struct CaptureRequest(std::sync::Mutex<Vec<lattice_llm::ChatRequest>>);
        #[async_trait::async_trait]
        impl LlmClient for CaptureRequest {
            async fn stream_chat(
                &self,
                request: lattice_llm::ChatRequest,
                control: lattice_llm::StreamControl,
            ) -> std::result::Result<lattice_llm::LlmStream, Box<dyn std::error::Error + Send + Sync>>
            {
                let origin = lattice_conversation::ReplayOrigin {
                    protocol: Protocol::ChatCompletions,
                    provider_id: request.target.model.provider().clone(),
                };
                self.0.lock().unwrap().push(request);
                Ok(lattice_llm::testing::script_stream(
                    origin,
                    happy_events(),
                    control,
                ))
            }
        }

        let captured = Arc::new(CaptureRequest(std::sync::Mutex::new(Vec::new())));
        let fixture = setup_fixture(captured.clone(), &[]);
        let mut request = fixture.request.clone();
        // 不显式注入 tools，走 default_tool_executor() 选择路径。
        request.tools = None;

        let (detail, _) = fixture.run(request).await;
        assert_eq!(detail.summary.outcome, PublicTurnOutcome::Completed);
        let frozen = fixture
            .store
            .begin_read()
            .unwrap()
            .turn_config(detail.session_id, detail.turn_id)
            .unwrap();
        assert!(
            frozen.catalog.entries.is_empty(),
            "未设置环境变量时冻结目录应为空"
        );
        let seen = captured.0.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert!(
            seen[0].tools.is_empty(),
            "NoTools 下模型请求不得携带工具定义"
        );
    }

    #[tokio::test]
    async fn fake_tool_executor_supports_read_append_oversize_fail_and_indeterminate() {
        let temp = tempfile::tempdir().unwrap();
        let marker_file = temp.path().join("markers.log");
        let executor = Arc::new(FakeToolExecutor::from_env_value("alpha, beta"));

        let llm = Arc::new(
            FakeStreamingLlmClient::builder()
                .push_events(tool_call_events(&[
                    (
                        "c1",
                        "alpha",
                        json!({"action": "read", "value": {"answer": 42}}),
                    ),
                    (
                        "c2",
                        "beta",
                        json!({
                            "action": "append_marker",
                            "path": marker_file.to_str().unwrap(),
                            "line": "step-0\n"
                        }),
                    ),
                    ("c3", "alpha", json!({"action": "oversize"})),
                    (
                        "c4",
                        "beta",
                        json!({
                            "action": "fail",
                            "code": "simulated_error",
                            "message": "模拟失败"
                        }),
                    ),
                ]))
                .push_events(tool_call_events(&[(
                    "c5",
                    "alpha",
                    json!({"action": "indeterminate"}),
                )]))
                .build(),
        );
        let fixture = setup_fixture(llm, &[]);
        let mut request = fixture.request.clone();
        request.tools = Some(executor);

        let (detail, _) = fixture.run(request).await;
        assert_eq!(
            std::fs::read_to_string(&marker_file).unwrap(),
            "step-0\n",
            "append_marker 应向目标路径写入标记内容"
        );
        assert_eq!(detail.steps.len(), 2);
        assert_eq!(detail.steps[0].end, StepEnd::Continue);
        let step0 = &detail.steps[0].slots;
        assert_eq!(step0.len(), 4);
        assert_eq!(step0[0].outcome, ToolResultOutcome::Succeeded);
        assert_eq!(step0[1].outcome, ToolResultOutcome::Succeeded);
        assert!(matches!(
            &step0[2].material,
            MaterialSummary::OutputTooLarge { .. }
        ));
        assert_eq!(
            step0[3].reason,
            Some(SlotReason::Failed {
                code: "simulated_error".into(),
            })
        );
        let view = fixture
            .store
            .begin_read()
            .unwrap()
            .view(detail.session_id, 10)
            .unwrap();
        let ConversationEntry::Step { content, .. } = &view.entries[1] else {
            panic!("expected step entry")
        };
        assert_eq!(
            content.step_content().unwrap().results[3].payload,
            ResultPayload::Failed(ToolError {
                code: "simulated_error".into(),
                message: "模拟失败".into(),
            })
        );

        assert_eq!(detail.summary.outcome, PublicTurnOutcome::Failed);
        assert_eq!(
            detail.steps[1].slots[0].reason,
            Some(SlotReason::Indeterminate(IndeterminateCause::Reported))
        );
    }

    #[tokio::test]
    async fn fake_tool_executor_block_action_observes_cancel_and_deadline() {
        let temp = tempfile::tempdir().unwrap();
        let ready = temp.path().join("ready.marker");
        let executor: Arc<dyn crate::ToolExecutor> =
            Arc::new(FakeToolExecutor::from_env_value("blocker"));

        // 1. 阻塞期间收到取消信号 -> 返回 Cancelled。
        let llm_cancel = Arc::new(
            FakeStreamingLlmClient::builder()
                .push_events(tool_call_events(&[(
                    "b1",
                    "blocker",
                    json!({
                        "action": "block",
                        "ready_path": ready.to_str().unwrap(),
                    }),
                )]))
                .build(),
        );
        let fixture = setup_fixture(llm_cancel, &[]);
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut req_cancel = fixture.request.clone();
        req_cancel.tools = Some(executor.clone());
        req_cancel.cancel_flag = Some(cancel.clone());
        let (execution, _) = fixture.start(req_cancel);
        execution.acceptance.await.unwrap().unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !ready.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("等待假工具进入阻塞状态超时");
        cancel.store(true, Ordering::Release);
        let crate::TurnCompletion::Closed(cancelled_detail) =
            execution.completion.await.unwrap().unwrap();
        assert_eq!(
            cancelled_detail.summary.outcome,
            PublicTurnOutcome::Cancelled
        );
        assert_eq!(
            cancelled_detail.steps[0].slots[0].reason,
            Some(SlotReason::Indeterminate(IndeterminateCause::Cancelled))
        );

        // 2. 阻塞期间触发轮次 deadline -> 返回 Deadline，槽位保持 Started / NoResult。
        let llm_deadline = Arc::new(
            FakeStreamingLlmClient::builder()
                .push_events(tool_call_events(&[
                    ("d1", "blocker", json!({"action": "block"})),
                    ("d2", "blocker", json!({"action": "read"})),
                ]))
                .build(),
        );
        let mut fixture_dl = setup_fixture(llm_deadline, &[]);
        fixture_dl
            .request
            .loaded_profile
            .as_mut()
            .unwrap()
            .profile
            .llm
            .timeout_secs = crate::TurnTimeout::new(1).unwrap();
        let mut req_dl = fixture_dl.request.clone();
        req_dl.tools = Some(executor);
        let (dl_detail, _) = fixture_dl.run(req_dl).await;
        assert_eq!(dl_detail.summary.outcome, PublicTurnOutcome::Failed);
        assert_eq!(
            dl_detail.summary.notice.as_ref().unwrap().text,
            "turn deadline exceeded"
        );
        assert_eq!(
            dl_detail.steps[0].end,
            StepEnd::Stop {
                cause: StopCause::Deadline
            }
        );
        assert_eq!(dl_detail.steps[0].slots[0].progress, SlotState::Started);
        assert_eq!(
            dl_detail.steps[0].slots[0].reason,
            Some(SlotReason::Indeterminate(IndeterminateCause::NoResult))
        );
        assert_eq!(dl_detail.steps[0].slots[1].progress, SlotState::Registered);
    }
}
