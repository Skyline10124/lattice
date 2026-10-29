// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
#![allow(dead_code)]

//! Shared direct-Component integration-test helpers.

use lancet_protocol::{CepError, InvocationId, ModelAttemptKey, SessionId, TurnId};
use lattice_conversation::{
    MessagePart, MessageRole, MessageSemantic, NativeMessage, Protocol, UnboundToolCall,
};
use lattice_session::store::{
    AcceptSession, AcceptTurn, AgentSelection, AttemptOrigin, AttemptProtocol, CloseStep,
    CompleteReply, DispatchStop, FrozenTurnConfig, SessionExecutionLock, SessionStore,
    StartedInvocation, StepKey, TerminalInput, ToolCatalogEntry, ToolCatalogSnapshot,
    TurnExecutionLimits, prepare_terminal,
};

mod git_fixture;
use git_fixture::init_fixture_repo;
use std::ops::Deref;
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub struct ReservedRuntime {
    runtime: Arc<lancet_api::LancetRuntime>,
    client: ReservedClient,
    _state: Arc<tempfile::TempDir>,
}
impl ReservedRuntime {
    pub fn build(
        mut config: lancet_api::LancetRuntimeConfig,
    ) -> Result<Self, lancet_api::LancetError> {
        let state = Arc::new(
            tempfile::TempDir::new()
                .map_err(|e| lancet_api::LancetError::Internal(e.to_string()))?,
        );
        if matches!(
            config.workspace_history,
            lancet_api::WorkspaceHistoryBacking::Disabled
        ) {
            config.workspace_history = lancet_api::WorkspaceHistoryBacking::for_project_in(
                state.path(),
                &config.local_workspace_root,
            )
            .map_err(|e| lancet_api::LancetError::Internal(e.to_string()))?;
            lancet_api::WorkspaceHistory::create(config.workspace_history.clone())
                .map_err(|e| lancet_api::LancetError::Internal(e.to_string()))?;
        }
        let project = lattice_session::ProjectStoreKey::derive(&config.local_workspace_root)
            .map_err(|e| lancet_api::LancetError::Internal(e.to_string()))?;
        // 每个测试运行时持独立文件库；工作区历史可共用，但不重复打开同一 redb。
        let store = Arc::new(
            SessionStore::create(&lattice_session::StateLayout::in_state_root(state.path()))
                .map_err(|e| lancet_api::LancetError::Internal(e.to_string()))?,
        );
        let mut runtime = lancet_api::LancetRuntime::build(config)?;
        let mut events = runtime.take_events();
        tokio::spawn(async move { while events.recv().await.is_some() {} });
        let catalog = ToolCatalogSnapshot::new(
            "integration-runtime".into(),
            runtime
                .tool_definitions()
                .iter()
                .map(|t| ToolCatalogEntry {
                    name: t.name.clone(),
                    descriptor_identity: t.descriptor_identity.clone(),
                    description: t.description.clone(),
                    input_schema: t.parameters.clone(),
                })
                .collect(),
        )
        .map_err(|e| lancet_api::LancetError::Internal(e.to_string()))?;
        let runtime = Arc::new(runtime);
        let client = ReservedClient {
            runtime: runtime.clone(),
            store,
            project,
            catalog,
            _state: state.clone(),
        };
        Ok(Self {
            runtime,
            client,
            _state: state,
        })
    }
    pub fn client(&self) -> ReservedClient {
        self.client.clone()
    }
}
impl Deref for ReservedRuntime {
    type Target = lancet_api::LancetRuntime;
    fn deref(&self) -> &Self::Target {
        &self.runtime
    }
}
#[derive(Clone)]
pub struct ReservedClient {
    runtime: Arc<lancet_api::LancetRuntime>,
    store: Arc<SessionStore>,
    project: lattice_session::ProjectStoreKey,
    catalog: ToolCatalogSnapshot,
    _state: Arc<tempfile::TempDir>,
}
impl ReservedClient {
    fn started(
        &self,
        method: String,
        params: serde_json::Value,
    ) -> (StartedInvocation, SessionExecutionLock, StepKey) {
        let sid = SessionId::parse(&uuid::Uuid::new_v4().simple().to_string()).unwrap();
        let tid = TurnId::parse(&uuid::Uuid::new_v4().to_string()).unwrap();
        let id = InvocationId::parse(&uuid::Uuid::new_v4().to_string()).unwrap();
        let lock = self.store.try_lock_session(sid).unwrap();
        self.store
            .accept_turn(
                &lock,
                AcceptTurn {
                    session: AcceptSession::New { id: sid },
                    turn_id: tid,
                    project_store_key: self.project,
                    input: "component fixture".into(),
                    accepted_at: chrono::Utc::now(),
                    agent_selection: AgentSelection::Explicit,
                    config: FrozenTurnConfig {
                        agent_name: "test".into(),
                        model: "test".into(),
                        provider: "openai-compatible".into(),
                        system_prompt: None,
                        catalog: self.catalog.clone(),
                        limits: TurnExecutionLimits {
                            timeout_secs: 300,
                            max_model_steps: 32,
                            max_model_attempts: 1,
                        },
                    },
                },
            )
            .unwrap();
        let key = ModelAttemptKey {
            turn_id: tid,
            model_step_ordinal: 0,
            attempt_ordinal: 0,
        };
        let origin = AttemptOrigin {
            protocol: AttemptProtocol::ChatCompletions,
            provider_id: "openai-compatible".into(),
            base_url: "https://provider.example".into(),
        };
        self.store
            .start_request(&lock, key, origin.clone(), chrono::Utc::now())
            .unwrap();
        let registered = self
            .store
            .register_calls(
                &lock,
                CompleteReply {
                    key,
                    origin,
                    usage: None,
                    message: NativeMessage::new(
                        MessageRole::Assistant,
                        Some(lattice_conversation::ReplayOrigin {
                            protocol: Protocol::ChatCompletions,
                            provider_id: "openai-compatible".into(),
                            base_url: "https://provider.example".into(),
                        }),
                        vec![MessagePart::semantic_only(MessageSemantic::ToolCall {
                            call: UnboundToolCall {
                                provider_call_id: Some("fixture-call".into()),
                                name: method,
                                arguments: params,
                            },
                        })],
                    ),
                },
                &[id],
                chrono::Utc::now(),
            )
            .unwrap();
        (
            self.store
                .start_invocation(&lock, &registered.bindings[0], chrono::Utc::now())
                .unwrap(),
            lock,
            registered.key,
        )
    }
    async fn dispatch(
        &self,
        method: String,
        params: serde_json::Value,
        cancel: Option<Arc<AtomicBool>>,
    ) -> lancet_api::ExecutionReceipt {
        let client = self.clone();
        let method_for_start = method.clone();
        let params_for_start = params.clone();
        let (started, lock, key) =
            tokio::task::spawn_blocking(move || client.started(method_for_start, params_for_start))
                .await
                .unwrap();
        let binding = started.binding().clone();
        let receipt = self.runtime.dispatch(started, method, params, cancel).await;
        let terminal =
            prepare_terminal(&binding, receipt.result.clone(), receipt.evidence.clone()).unwrap();
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            store
                .finish_invocation(
                    &lock,
                    TerminalInput {
                        invocation_id: binding.invocation_id,
                        terminal,
                    },
                    chrono::Utc::now(),
                )
                .unwrap();
            store
                .close_step(
                    &lock,
                    CloseStep::StopExecuting {
                        key,
                        stop: DispatchStop::ProcessExit,
                    },
                    chrono::Utc::now(),
                )
                .unwrap();
        })
        .await
        .unwrap();
        receipt
    }
    pub async fn call_receipt(
        &self,
        method: impl Into<String>,
        params: serde_json::Value,
    ) -> Result<lancet_api::ExecutionReceipt, CepError> {
        Ok(self.dispatch(method.into(), params, None).await)
    }
    pub async fn call_raw(
        &self,
        method: impl Into<String>,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, CepError> {
        self.call_receipt(method, params).await?.result
    }
    pub fn start_call_raw(
        &self,
        method: String,
        params: serde_json::Value,
    ) -> (
        impl std::future::Future<Output = Result<serde_json::Value, CepError>> + Send + 'static,
        TestCallControl,
    ) {
        let client = self.clone();
        let flag = Arc::new(AtomicBool::new(false));
        let control = TestCallControl { flag: flag.clone() };
        (
            async move { client.dispatch(method, params, Some(flag)).await.result },
            control,
        )
    }
}

pub struct TestCallControl {
    flag: Arc<AtomicBool>,
}

impl TestCallControl {
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Relaxed);
    }
}

pub fn init_git_repo(root: &Path) {
    let mut files = Vec::new();
    collect_files(root, root, &mut files);
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(path, content)| (path.as_str(), content.as_str()))
        .collect();
    let _ = init_fixture_repo(root, &refs);
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_name() == ".git" {
            continue;
        }
        if path.is_dir() {
            collect_files(root, &path, out);
        } else if path.is_file() {
            let relative = path
                .strip_prefix(root)
                .expect("under root")
                .to_string_lossy()
                .into_owned();
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            out.push((relative, content));
        }
    }
}
