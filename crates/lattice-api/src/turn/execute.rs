// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

#[cfg(any(test, feature = "test-utils"))]
use futures::StreamExt;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;

use super::cep::{ProductionTurnDepsInput, build_production_turn_deps};
#[cfg(any(test, feature = "test-utils"))]
use super::collector::RunTurnCollector;
#[cfg(any(test, feature = "test-utils"))]
use super::coordinator::IdSource;
use super::coordinator::{CoordinatorStoreIo, RandomIdSource, TurnCoordinator};
use super::llm::RealLlmClient;
#[cfg(any(test, feature = "test-utils"))]
use super::mapping::resolve_project_root;
use super::observer::ObserverSink;
use super::types::{RunTurnRequest, TurnAcceptance, TurnCompletion, TurnExecution};
#[cfg(any(test, feature = "test-utils"))]
use super::types::{RunTurnResult, TurnEvent, TurnEventStream};
use crate::session::{ApiError, Result, SessionApi};

#[derive(Debug, Clone)]
pub(super) struct ProfileSnapshotMetadata {
    pub agent_name: String,
    pub profile_snapshot_json: String,
}

pub(super) fn build_profile_snapshot_metadata(
    loaded: &lattice_profile::LoadedAgentProfile,
) -> Result<ProfileSnapshotMetadata> {
    let value = serde_json::json!({ "agent_profile": loaded.profile, "system_prompt": loaded.system_prompt });
    let profile_snapshot_json = serde_json::to_string(&value).map_err(|err| {
        ApiError::Internal(format!("failed to serialize profile snapshot: {err}"))
    })?;
    Ok(ProfileSnapshotMetadata {
        agent_name: loaded.profile.agent.name.clone(),
        profile_snapshot_json,
    })
}

pub const STREAM_IDLE_TIMEOUT_SECS: u64 = 300;

struct LockedTurn {
    session_id: String,
    project_store_key: lattice_session::ProjectStoreKey,
    layout: lattice_session::StateLayout,
    execution_lock: Arc<lattice_session::FlockGuard>,
    coordinator: Arc<TurnCoordinator>,
    workspace_history: Arc<lancet_api::WorkspaceHistory>,
}

async fn lock_turn(
    session: SessionApi,
    session_id: String,
    workspace: Option<crate::root::LocalWorkspaceRoot>,
) -> Result<LockedTurn> {
    tokio::task::spawn_blocking(move || {
        let (session_id, execution_lock) = session.try_lock_execution(&session_id)?;
        let (_, project_store_key) =
            session.turn_project_binding_locked(&session_id, &execution_lock)?;
        if workspace.is_some_and(|root| root.store_key() != project_store_key) {
            return Err(ApiError::Conflict(
                "requested Local Workspace differs from the Session binding".into(),
            ));
        }
        let layout = session.state_layout().ok_or_else(|| {
            ApiError::Internal("Turn requires a StateLayout-backed SessionApi".into())
        })?;
        // 恢复只打开领域存储，不组合 Component，也不读取当前 profile 或 provider 凭据。
        let invocation_store = Arc::new(
            lancet_audit::invocation_store::InvocationStore::open(&layout, &project_store_key)
                .map_err(|err| ApiError::Internal(format!("open invocation store: {err}")))?,
        );
        let workspace_history = Arc::new(
            lancet_api::WorkspaceHistory::open(lancet_api::WorkspaceHistoryBacking::for_key(
                layout.clone(),
                project_store_key,
            ))
            .map_err(|err| ApiError::Internal(format!("workspace history setup: {err}")))?,
        );
        let coordinator = Arc::new(TurnCoordinator::new(
            CoordinatorStoreIo::new(
                session.git_store_for_coordinator()?,
                invocation_store,
                project_store_key,
            )
            .map_err(|err| ApiError::Internal(format!("compose TurnCoordinator: {err}")))?,
            Arc::new(RandomIdSource),
        ));
        Ok(LockedTurn {
            session_id,
            project_store_key,
            layout,
            execution_lock: Arc::new(execution_lock),
            coordinator,
            workspace_history,
        })
    })
    .await
    .map_err(|err| ApiError::Internal(format!("prepare Turn stores: {err}")))?
}

async fn reconcile(
    locked: &LockedTurn,
    target: &std::sync::OnceLock<lancet_protocol::TurnId>,
) -> Result<Option<lattice_session::TurnCommit>> {
    let pending = locked
        .coordinator
        .io
        .read_active_turn(locked.session_id.clone())
        .await
        .map_err(reconcile_error)?;
    if let Some(pending) = pending {
        // 在全 Turn 锁下验证日志后即报告归属；后续发布失败也属于这个旧 Turn。
        let _ = target.set(pending.snapshot.turn_id);
    }
    locked
        .coordinator
        .reconcile_pending(
            locked.session_id.clone(),
            Arc::clone(&locked.execution_lock),
            Arc::clone(&locked.workspace_history),
        )
        .await
        .map_err(reconcile_error)
}

fn reconcile_error(error: super::coordinator::CoordinatorIoError) -> ApiError {
    match error {
        super::coordinator::CoordinatorIoError::EvidenceDurabilityFailed { attempts } => {
            ApiError::RecoveryRequired(crate::session::RecoveryRequired::EvidenceDurabilityFailed(
                crate::session::EvidenceDurabilityFailed { attempts },
            ))
        }
        other => ApiError::Internal(format!("reconcile pending Turn: {other}")),
    }
}

/// 独立 closure-only 恢复；忙时立即拒绝，无 pending 时不创建 Turn。
#[cfg(test)]
pub(super) async fn recover_turn(
    session: SessionApi,
    session_id: String,
) -> Result<Option<lattice_session::TurnCommit>> {
    recover_turn_with_target(session, session_id, Arc::default()).await
}

pub(crate) async fn recover_turn_with_target(
    session: SessionApi,
    session_id: String,
    target: Arc<std::sync::OnceLock<lancet_protocol::TurnId>>,
) -> Result<Option<lattice_session::TurnCommit>> {
    let locked = lock_turn(session, session_id, None).await?;
    reconcile(&locked, &target).await
}

pub(super) struct ExecutionFeedback {
    pub(super) acceptance: Option<oneshot::Sender<Result<TurnAcceptance>>>,
    pub(super) observer: ObserverSink,
    pub(super) verified_turn: Arc<std::sync::OnceLock<lancet_protocol::TurnId>>,
}

impl ExecutionFeedback {
    pub(super) fn accept(&mut self, acceptance: TurnAcceptance) {
        if let Some(sender) = self.acceptance.take() {
            let _ = sender.send(Ok(acceptance));
        }
    }
}

/// 立即接管准备与执行任务。身份、规范工作区和连接材料由提交端提供；None 连接只适用于找回或测试注入。
/// 接管不等于持久接受。后台同 ID 的并发连接应共享此句柄的回执，而非再次启动任务。
pub fn start_turn(
    session: SessionApi,
    turn_id: lancet_protocol::TurnId,
    workspace: crate::root::LocalWorkspaceRoot,
    request: RunTurnRequest,
    connection: Option<Arc<lattice_llm::LlmConnection>>,
) -> TurnExecution {
    start_execution(
        session,
        turn_id,
        workspace,
        request,
        connection,
        false,
        Arc::default(),
    )
}

pub(crate) fn start_execution(
    session: SessionApi,
    turn_id: lancet_protocol::TurnId,
    workspace: crate::root::LocalWorkspaceRoot,
    mut request: RunTurnRequest,
    connection: Option<Arc<lattice_llm::LlmConnection>>,
    capture_env: bool,
    verified_turn: Arc<std::sync::OnceLock<lancet_protocol::TurnId>>,
) -> TurnExecution {
    let cancel_flag = request
        .cancel_flag
        .get_or_insert_with(|| Arc::new(AtomicBool::new(false)))
        .clone();
    let (events_tx, events_rx) = mpsc::channel(super::live::TURN_EVENT_CHANNEL_CAPACITY);
    let (acceptance_tx, acceptance) = oneshot::channel();
    let (completion_tx, completion) = oneshot::channel();
    let event_sequence = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let sequence = event_sequence.clone();
    tokio::spawn(async move {
        let mut feedback = ExecutionFeedback {
            acceptance: Some(acceptance_tx),
            observer: ObserverSink::sequenced(events_tx, sequence),
            verified_turn,
        };
        let result = execute_owned(
            session,
            turn_id,
            workspace,
            request,
            connection,
            capture_env,
            &mut feedback,
        )
        .await;
        if let (Some(sender), Err(error)) = (feedback.acceptance.take(), &result) {
            let _ = sender.send(Err(error.clone()));
        }
        let _ = completion_tx.send(result);
    });
    TurnExecution {
        acceptance,
        completion,
        events: Box::pin(ReceiverStream::new(events_rx)),
        event_sequence,
        cancel_flag,
    }
}

async fn execute_owned(
    session: SessionApi,
    turn_id: lancet_protocol::TurnId,
    workspace: crate::root::LocalWorkspaceRoot,
    request: RunTurnRequest,
    mut connection: Option<Arc<lattice_llm::LlmConnection>>,
    capture_env: bool,
    feedback: &mut ExecutionFeedback,
) -> Result<TurnCompletion> {
    let locked = lock_turn(
        session.clone(),
        request.session_id.clone(),
        Some(workspace.clone()),
    )
    .await?;
    let existing = {
        let store = session.git_store_for_coordinator()?;
        let id = locked.session_id.clone();
        let input = request.input.clone();
        let verified_turn = feedback.verified_turn.clone();
        let lock = Arc::clone(&locked.execution_lock);
        tokio::task::spawn_blocking(move || -> Result<_> {
            let view = store
                .read_view(&id, Some(turn_id))
                .map_err(crate::session::map_session_error)?;
            let Some(existing) = view.requested_turn else {
                return Ok(None);
            };
            let facts = existing.started();
            let expected = lattice_conversation::NativeMessage::new(
                lattice_conversation::MessageRole::User,
                None,
                vec![lattice_conversation::MessagePart::semantic_only(
                    lattice_conversation::MessageSemantic::Text { text: input },
                )],
            );
            if facts.user_message != expected {
                return Err(ApiError::Conflict(
                    "TurnId is already bound to different input".into(),
                ));
            }
            // 已有 Turn 的输入与项目绑定已核验；确认屏障之前即确定诊断与订阅归属。
            let _ = verified_turn.set(turn_id);
            // 找回也须确认原发布的屏障；不覆盖冻结配置、不请求模型、不执行恢复。
            let confirmation = match &existing {
                lattice_session::StoredTurn::Pending(_) => {
                    store.begin_pending_turn(facts.clone(), &lock).map(|_| ())
                }
                lattice_session::StoredTurn::Committed { .. } => {
                    store.finalize_turn(&id, &turn_id, &lock).map(|_| ())
                }
            }
            .map_err(crate::session::map_session_error);
            Ok(Some((existing, confirmation.err())))
        })
        .await
        .map_err(|err| ApiError::Internal(format!("confirm existing Turn: {err}")))??
    };
    if let Some((existing, error)) = existing {
        if let Some(error) = error {
            feedback.accept(TurnAcceptance::ConfirmationUnknown { turn_id });
            return Err(error);
        }
        feedback.accept(TurnAcceptance::Existing(Box::new(existing.clone())));
        return Ok(match existing {
            lattice_session::StoredTurn::Committed { commit, .. } => {
                TurnCompletion::Committed(Box::new(commit))
            }
            lattice_session::StoredTurn::Pending(_) => TurnCompletion::Pending(turn_id),
        });
    }
    // 旧任务只做关闭；新提交的观察、诊断与取消身份不能被它占用。
    reconcile(&locked, &std::sync::OnceLock::new()).await?;
    let loaded = request
        .loaded_profile
        .as_ref()
        .ok_or_else(|| ApiError::BadRequest("loaded active agent profile is required".into()))?;
    let injected_llm = request.injected_llm();
    if injected_llm.is_none() && connection.is_none() && capture_env {
        let target = lattice_llm::ModelTarget::new(&loaded.profile.agent.model);
        connection = Some(Arc::new(
            tokio::task::spawn_blocking(move || lattice_llm::LlmConnection::from_env(&target))
                .await
                .map_err(|err| ApiError::Internal(format!("freeze provider connection: {err}")))?
                .map_err(|err| ApiError::BadRequest(err.to_string()))?,
        ));
    }
    if let Some(connection) = &connection
        && connection.model().alias != loaded.profile.agent.model
    {
        return Err(ApiError::Conflict(
            "profile model differs from frozen connection".into(),
        ));
    }
    let (llm, provider): (Arc<dyn lattice_agent::LlmClient>, _) = match (injected_llm, &connection)
    {
        (Some((client, provider)), _) => (client, provider),
        (_, Some(connection)) => (
            Arc::new(RealLlmClient(connection.clone())),
            connection.model().provider.clone(),
        ),
        _ => {
            return Err(ApiError::BadRequest(
                "frozen provider connection is required".into(),
            ));
        }
    };
    let project_root = workspace.canonical_path().to_str().ok_or_else(|| {
        ApiError::BadRequest(
            "Local Workspace root must be valid UTF-8 for the runtime policy path".into(),
        )
    })?;
    let deps = build_production_turn_deps(ProductionTurnDepsInput {
        request: &request,
        project_root,
        project_store_key: locked.project_store_key,
        state_layout: locked.layout,
        enabled_tools: &loaded.profile.tools.enabled,
        tls_roots_der: connection
            .as_ref()
            .map_or(&[], |connection| connection.tls_roots_der()),
        llm,
        lancet: Some(&loaded.profile.lancet),
    })
    .await?;
    let prepared = {
        let session = session.clone();
        let request = request.clone();
        let lock = Arc::clone(&locked.execution_lock);
        let provider = provider.as_str();
        tokio::task::spawn_blocking(move || {
            super::live::prepare_live_turn_locked(
                &session,
                &request,
                workspace.canonical_path(),
                &lock,
                provider,
            )
        })
        .await
        .map_err(|err| ApiError::Internal(format!("prepare live Turn: {err}")))??
    };
    super::live::run_live_turn_task(
        prepared,
        request,
        turn_id,
        deps,
        locked.coordinator,
        locked.execution_lock,
        feedback,
    )
    .await
    .map(|commit| TurnCompletion::Committed(Box::new(commit)))
}

#[cfg(any(test, feature = "test-utils"))]
fn committed_event(commit: &lattice_session::TurnCommit) -> TurnEvent {
    TurnEvent::TurnCommitted {
        session_id: commit.session_id.clone(),
        commit_id: commit.commit_id.clone(),
        outcome: commit.outcome.clone(),
        origin: commit.origin,
        workspace_history_checkpoint: commit.workspace_history_checkpoint.clone(),
    }
}

#[cfg(any(test, feature = "test-utils"))]
async fn legacy_execution(session: SessionApi, request: RunTurnRequest) -> Result<TurnExecution> {
    let root = resolve_project_root(&request)?;
    let workspace =
        tokio::task::spawn_blocking(move || crate::root::LocalWorkspaceRoot::from_path(&root))
            .await
            .map_err(|err| ApiError::Internal(format!("resolve workspace: {err}")))??;
    let turn_id = RandomIdSource.new_turn_id();
    Ok(start_execution(
        session,
        turn_id,
        workspace,
        request,
        None,
        true,
        Arc::default(),
    ))
}

/// 测试复用同一已接管任务；调用者释放观察流不取消执行。
#[cfg(any(test, feature = "test-utils"))]
pub async fn stream_turn(session: SessionApi, request: RunTurnRequest) -> Result<TurnEventStream> {
    let execution = legacy_execution(session, request).await?;
    let acceptance = execution
        .acceptance
        .await
        .map_err(|_| ApiError::Internal("acceptance owner disappeared".into()))??;
    if matches!(acceptance, TurnAcceptance::ConfirmationUnknown { .. }) {
        return Err(ApiError::StoreUnavailable(
            "Turn acceptance confirmation unknown".into(),
        ));
    }
    Ok(Box::pin(execution.events.map(|item| item.event)))
}

/// 收集同一 live 核心；完成结果独立于末尾事件是否投递成功。
#[cfg(any(test, feature = "test-utils"))]
pub async fn run_turn(session: SessionApi, request: RunTurnRequest) -> Result<RunTurnResult> {
    let execution = legacy_execution(session, request).await?;
    execution
        .acceptance
        .await
        .map_err(|_| ApiError::Internal("acceptance owner disappeared".into()))??;
    let mut events = execution.events;
    let mut collector = RunTurnCollector::new();
    while let Some(event) = events.next().await {
        collector.observe(event.event)?;
    }
    let completed = execution
        .completion
        .await
        .map_err(|_| ApiError::Internal("completion owner disappeared".into()))??;
    let TurnCompletion::Committed(commit) = completed else {
        return Err(ApiError::StoreUnavailable("Turn remains pending".into()));
    };
    collector.observe(committed_event(&commit))?;
    collector.finish()
}
