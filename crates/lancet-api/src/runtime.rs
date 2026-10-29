// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 消费已提交的调用凭据，组合生成的 Component 执行器。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use lancet_authority::{CepPolicyConfig, CepTurnContext};
use lancet_protocol::ToolEvent;
use lancet_runtime::{
    CancellationSender, CatalogSelectionError, ComponentRuntime, ExecutionReceipt,
    RuntimeExecution, ToolDefinition, WorkspaceHistory, WorkspaceHistoryBacking,
    enabled_tool_definitions,
};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::config::LancetRuntimeConfig;
use crate::error::LancetError;

#[cfg(test)]
static STALL_NEXT_CANCELLATION_MONITOR: AtomicBool = AtomicBool::new(false);

/// One opaque, turn-scoped generated-Component runtime.
pub struct LancetRuntime {
    execution: RuntimeExecution,
    tool_definitions: Vec<ToolDefinition>,
    workspace_history: Arc<WorkspaceHistory>,
    events: Option<mpsc::UnboundedReceiver<ToolEvent>>,
}

impl LancetRuntime {
    /// 组合生成目录，并严格打开项目工作区历史。
    pub fn build(config: LancetRuntimeConfig) -> Result<Self, LancetError> {
        let LancetRuntimeConfig {
            local_workspace_root,
            enabled_tools,
            mut policy,
            capability_admission,
            tls_roots_der,
            workspace_history,
        } = config;

        policy.project_root = validate_project_store_bindings(
            &local_workspace_root,
            &policy.project_root,
            &workspace_history,
        )?;

        let components = ComponentRuntime::shared()
            .map_err(|error| LancetError::Internal(format!("component composition: {error}")))?;
        let tool_definitions = enabled_tool_definitions(&components, &enabled_tools)
            .map_err(map_catalog_selection_error)?;
        lancet_runtime::validate_policy_against_catalog(
            components
                .descriptors()
                .flat_map(|descriptor| descriptor.capabilities.iter())
                .map(|capability| capability.name.as_str()),
            &capability_admission,
        )
        .map_err(|error| LancetError::Config(format!("capability admission: {error}")))?;

        let cep_policy = CepPolicyConfig {
            project_root: policy.project_root.clone(),
            observation: policy.observation.clone(),
            network: policy.network.clone(),
            capability_admission,
        };
        let workspace_history =
            Arc::new(WorkspaceHistory::open(workspace_history).map_err(|error| {
                LancetError::Internal(format!("workspace history setup: {error}"))
            })?);
        let (execution, events) = RuntimeExecution::new(
            components,
            Arc::new(CepTurnContext::from_policy(cep_policy)),
            &policy,
            Arc::clone(&workspace_history),
            &tls_roots_der,
        )
        .map_err(LancetError::Config)?;

        Ok(Self {
            execution,
            tool_definitions,
            workspace_history,
            events: Some(events),
        })
    }

    /// Frozen provider-visible definitions from generated descriptors.
    pub fn tool_definitions(&self) -> &[ToolDefinition] {
        &self.tool_definitions
    }

    /// Take the sole event receiver for this Turn runtime.
    pub fn take_events(&mut self) -> mpsc::UnboundedReceiver<ToolEvent> {
        self.events
            .take()
            .expect("Tool event receiver may be taken only once")
    }

    /// 文件变更的私有历史；不参与 Turn 关闭。
    pub fn workspace_history(&self) -> &Arc<WorkspaceHistory> {
        &self.workspace_history
    }

    /// 消费成功提交 Started 后的唯一执行凭据；不重新持久化或猜测调用终态。
    pub async fn dispatch(
        &self,
        started: lattice_session::store::StartedInvocation,
        method: String,
        params: Value,
        cancel_flag: Option<Arc<AtomicBool>>,
    ) -> ExecutionReceipt {
        let binding = started.binding().clone();
        let (sender, cancellation) = CancellationSender::pair();
        let monitor = cancel_flag.as_ref().map(|flag| {
            let flag = Arc::clone(flag);
            let sender = sender.clone();
            #[cfg(test)]
            let stall_monitor = STALL_NEXT_CANCELLATION_MONITOR.swap(false, Ordering::SeqCst);
            tokio::spawn(async move {
                #[cfg(test)]
                if stall_monitor {
                    std::future::pending::<()>().await;
                }
                while !flag.load(Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                sender.cancel();
            })
        });
        if cancel_flag
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
        {
            sender.cancel();
        }
        let _guard = DispatchGuard { sender, monitor };
        self.execution
            .call(binding, method, params, cancellation)
            .await
    }
}

struct DispatchGuard {
    sender: CancellationSender,
    monitor: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for DispatchGuard {
    fn drop(&mut self) {
        self.sender.cancel();
        if let Some(monitor) = self.monitor.take() {
            monitor.abort();
        }
    }
}

fn map_catalog_selection_error(error: CatalogSelectionError) -> LancetError {
    LancetError::UnknownTools {
        unknown: error.unknown,
        available: error.available,
    }
}

fn validate_project_store_bindings(
    local_workspace_root: &std::path::Path,
    policy_root: &std::path::Path,
    workspace_history: &WorkspaceHistoryBacking,
) -> Result<std::path::PathBuf, LancetError> {
    let canonical_workspace = std::fs::canonicalize(local_workspace_root).map_err(|error| {
        LancetError::Config(format!(
            "cannot canonicalize Local Workspace root '{}': {error}",
            local_workspace_root.display()
        ))
    })?;
    let canonical_policy = std::fs::canonicalize(policy_root).map_err(|error| {
        LancetError::Config(format!(
            "cannot canonicalize policy project root '{}': {error}",
            policy_root.display()
        ))
    })?;
    if !canonical_policy.starts_with(&canonical_workspace) {
        return Err(LancetError::Config(format!(
            "policy project root '{}' resolves outside Local Workspace root '{}'",
            canonical_policy.display(),
            canonical_workspace.display()
        )));
    }

    let expected_key =
        lancet_runtime::ProjectStoreKey::derive(&canonical_workspace).map_err(|error| {
            LancetError::Config(format!("derive Local Workspace project key: {error}"))
        })?;
    let (_, history_key) = project_backing(workspace_history, "WorkspaceHistory")?;
    if history_key != expected_key {
        return Err(LancetError::Config(
            "project store key does not match canonical Local Workspace".into(),
        ));
    }
    Ok(canonical_policy)
}

fn project_backing<'a>(
    backing: &'a WorkspaceHistoryBacking,
    name: &str,
) -> Result<
    (
        &'a lancet_runtime::StateLayout,
        lancet_runtime::ProjectStoreKey,
    ),
    LancetError,
> {
    match backing {
        WorkspaceHistoryBacking::Project { layout, key } => Ok((layout, *key)),
        WorkspaceHistoryBacking::Disabled => Err(LancetError::Config(format!(
            "production runtime requires a project {name} backing"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_protocol::{
        CepErrorCode, InvocationId, ModelAttemptKey, SessionId, TurnId, cell::Trust,
        evidence::CapabilityAdmissionPolicy,
    };
    use lattice_conversation::{
        MessagePart, MessageRole, MessageSemantic, NativeMessage, Protocol, UnboundToolCall,
    };
    use lattice_session::store::*;
    use tempfile::TempDir;

    static DISPATCH_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct TestRuntime {
        runtime: LancetRuntime,
        db: SessionStore,
        workspace: TempDir,
        _state: TempDir,
    }

    fn test_runtime(enabled: &[&str]) -> TestRuntime {
        let workspace = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(workspace.path()).unwrap();
        let backing = WorkspaceHistoryBacking::for_project_in(state.path(), &root).unwrap();
        WorkspaceHistory::create(backing.clone()).unwrap();
        let db =
            SessionStore::create(&lattice_store::StateLayout::in_state_root(state.path())).unwrap();
        let runtime = LancetRuntime::build(LancetRuntimeConfig {
            local_workspace_root: root.clone(),
            enabled_tools: enabled.iter().map(|s| s.to_string()).collect(),
            policy: lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(
                root,
            ),
            capability_admission: CapabilityAdmissionPolicy {
                enabled_tools: enabled.iter().map(|s| s.to_string()).collect(),
                denied_capabilities: Default::default(),
                max_boundary: Trust::Ex,
            },
            tls_roots_der: Vec::new(),
            workspace_history: backing,
        })
        .unwrap();
        TestRuntime {
            runtime,
            db,
            workspace,
            _state: state,
        }
    }

    fn start(
        env: &TestRuntime,
        n: u64,
        params: &Value,
        wrong_descriptor: bool,
    ) -> StartedInvocation {
        let sid = SessionId::parse(&format!("{n:032x}")).unwrap();
        let tid = TurnId::parse(&format!("00000000-0000-4000-8000-{n:012x}")).unwrap();
        let now = "2026-09-27T00:00:00Z".parse().unwrap();
        let lock = env.db.try_lock_session(sid).unwrap();
        let tool = env
            .runtime
            .tool_definitions()
            .iter()
            .find(|t| t.name == "mutate__create")
            .unwrap();
        let config = FrozenTurnConfig {
            agent_name: "test".into(),
            model: "model".into(),
            provider: "openai".into(),
            system_prompt: None,
            catalog: ToolCatalogSnapshot::new(
                "build".into(),
                vec![ToolCatalogEntry {
                    name: tool.name.clone(),
                    descriptor_identity: if wrong_descriptor {
                        "a".repeat(64)
                    } else {
                        tool.descriptor_identity.clone()
                    },
                    description: tool.description.clone(),
                    input_schema: tool.parameters.clone(),
                }],
            )
            .unwrap(),
            limits: TurnExecutionLimits {
                timeout_secs: 30,
                max_model_steps: 32,
                max_model_attempts: 1,
            },
        };
        env.db
            .accept_turn(
                &lock,
                AcceptTurn {
                    session: AcceptSession::New { id: sid },
                    turn_id: tid,
                    project_store_key: lattice_store::ProjectStoreKey::derive(env.workspace.path())
                        .unwrap(),
                    input: "create".into(),
                    config,
                    accepted_at: now,
                    agent_selection: AgentSelection::Explicit,
                },
            )
            .unwrap();
        let key = ModelAttemptKey {
            turn_id: tid,
            model_step_ordinal: 0,
            attempt_ordinal: 0,
        };
        let origin = lattice_session::store::AttemptOrigin {
            protocol: lattice_session::store::AttemptProtocol::ChatCompletions,
            provider_id: "openai".into(),
        };
        env.db
            .start_request(&lock, key, origin.clone(), now)
            .unwrap();
        let calls = env
            .db
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
                            provider_id: "openai".into(),
                        }),
                        vec![MessagePart::semantic_only(MessageSemantic::ToolCall {
                            call: UnboundToolCall {
                                provider_call_id: Some("call".into()),
                                name: "mutate__create".into(),
                                arguments: params.clone(),
                            },
                        })],
                    ),
                },
                &[InvocationId::parse(&format!("00000000-0000-4000-8000-{n:012x}")).unwrap()],
                now,
            )
            .unwrap();
        env.db
            .start_invocation(&lock, &calls.bindings[0], now)
            .unwrap()
    }

    #[tokio::test]
    async fn started_cancellation_is_synchronous() {
        let _guard = DISPATCH_TEST_LOCK.lock().await;
        let env = test_runtime(&["mutate__create"]);
        let params =
            serde_json::json!({"path":"cancelled.txt","content":"no\n","create_parents":false});
        let started = start(&env, 1, &params, false);
        STALL_NEXT_CANCELLATION_MONITOR.store(true, Ordering::SeqCst);
        let receipt = env
            .runtime
            .dispatch(
                started,
                "mutate__create".into(),
                params,
                Some(Arc::new(AtomicBool::new(true))),
            )
            .await;
        assert_eq!(
            receipt.result.unwrap_err().code,
            CepErrorCode::CallCancelled
        );
        assert!(!env.workspace.path().join("cancelled.txt").exists());
        assert_eq!(env.runtime.workspace_history().current().unwrap(), None);
        assert!(!receipt.evidence.admission.approved_capabilities.is_empty());
    }

    #[tokio::test]
    async fn started_dispatch_checks_binding_and_records_mutation() {
        let _guard = DISPATCH_TEST_LOCK.lock().await;
        let env = test_runtime(&["mutate__create"]);
        let params =
            serde_json::json!({"path":"created.txt","content":"yes\n","create_parents":false});
        for n in 1..=3 {
            let started = start(&env, n, &params, n == 3);
            let method = if n == 1 {
                "mutate__edit"
            } else {
                "mutate__create"
            };
            let actual = if n == 2 {
                serde_json::json!({"path":"other.txt"})
            } else {
                params.clone()
            };
            let receipt = env
                .runtime
                .dispatch(started, method.into(), actual, None)
                .await;
            assert_eq!(
                receipt.result.unwrap_err().code,
                CepErrorCode::SchemaMismatch
            );
            assert!(!env.workspace.path().join("created.txt").exists());
            assert_eq!(env.runtime.workspace_history().current().unwrap(), None);
        }
        let started = start(&env, 4, &params, false);
        let receipt = env
            .runtime
            .dispatch(started, "mutate__create".into(), params, None)
            .await;
        assert!(receipt.result.is_ok(), "{:?}", receipt.result);
        assert_eq!(
            std::fs::read(env.workspace.path().join("created.txt")).unwrap(),
            b"yes\n"
        );
        assert!(env.runtime.workspace_history().current().unwrap().is_some());
        assert!(!receipt.evidence.effects.effects.is_empty());
    }

    #[test]
    fn operator_facade_uses_supplied_snapshot() {
        let env = test_runtime(&["mutate__create"]);
        let project = lattice_store::ProjectStoreKey::derive(env.workspace.path()).unwrap();
        let read = env.db.begin_read().unwrap();
        let filter = AuditFilter {
            project_store_key: project,
            session_id: None,
            turn_id: None,
            method: None,
            outcome: None,
        };
        let params =
            serde_json::json!({"path":"created.txt","content":"yes\n","create_parents":false});
        let _started = start(&env, 1, &params, false);
        assert!(
            crate::query_audit(&read, &filter, None, 20)
                .unwrap()
                .rows
                .is_empty()
        );
        assert_eq!(
            crate::collect_status(&read, &project)
                .unwrap()
                .latest_change,
            ChangeSeq(0)
        );
        let fresh = env.db.begin_read().unwrap();
        let rows = crate::query_audit(&fresh, &filter, None, 20).unwrap().rows;
        assert_eq!(rows.len(), 1);
        assert!(matches!(rows[0].state, OperatorInvocationState::Started));
        assert_eq!(
            crate::collect_status(&fresh, &project).unwrap().recent,
            rows
        );
        assert_eq!(
            crate::tail_audit(&fresh, &filter, ChangeSeq(0), 20)
                .unwrap()
                .rows
                .len(),
            2
        );
        assert_eq!(
            crate::replay_audit(
                &fresh,
                &project,
                rows[0].binding.session_id,
                rows[0].binding.turn_id
            )
            .unwrap()
            .calls,
            rows
        );
        let other = lattice_store::ProjectStoreKey::from_canonical_path_bytes(b"/other");
        assert!(
            crate::collect_status(&fresh, &other)
                .unwrap()
                .recent
                .is_empty()
        );
    }
}
