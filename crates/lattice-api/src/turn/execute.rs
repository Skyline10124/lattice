// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::{
    cep::{ProductionTurnDepsInput, build_production_turn_deps},
    coordinator::{CoordinatorStoreIo, RandomIdSource, TurnCoordinator},
    observer::ObserverSink,
    types::*,
};
use crate::{
    root::LocalWorkspaceRoot,
    session::{ApiError, RequestConflict, Result, SessionApi},
};
use lancet_protocol::{SessionId, TurnId};
use lattice_session::{StateLayout, store::*};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::oneshot;

pub const STREAM_IDLE_TIMEOUT_SECS: u64 = 300;

pub(crate) struct ExecutionInput {
    pub session: SessionApi,
    pub layout: StateLayout,
    pub turn_id: TurnId,
    pub workspace: LocalWorkspaceRoot,
    pub request: RunTurnRequest,
    pub connection: Option<Arc<lattice_llm::LlmConnection>>,
    pub shutdown: Arc<AtomicBool>,
    pub observer: Arc<ObserverSink>,
    /// 新会话身份由提交方在执行开始前给定。
    pub new_session: Option<SessionId>,
}

/// 后台接管与持久接受不同。这个句柄的回执被释放也不撤销后台责任。
pub fn start_turn(
    session: SessionApi,
    layout: StateLayout,
    turn_id: TurnId,
    workspace: LocalWorkspaceRoot,
    request: RunTurnRequest,
    connection: Option<Arc<lattice_llm::LlmConnection>>,
) -> TurnExecution {
    let new_session = request
        .session_id
        .is_none()
        .then(crate::local::new_session_id);
    start_execution(ExecutionInput {
        session,
        layout,
        turn_id,
        workspace,
        request,
        connection,
        shutdown: Arc::default(),
        observer: Arc::default(),
        new_session,
    })
}

pub(crate) fn start_execution(mut input: ExecutionInput) -> TurnExecution {
    let cancel_flag = input
        .request
        .cancel_flag
        .get_or_insert_with(Arc::default)
        .clone();
    let (accepted, acceptance) = oneshot::channel();
    let (completed, completion) = oneshot::channel();
    tokio::spawn(async move {
        let mut accepted = Some(accepted);
        let result = execute_owned(input, &mut accepted).await;
        if let (Some(sender), Err(error)) = (accepted.take(), &result) {
            let _ = sender.send(Err(error.clone()));
        }
        let _ = completed.send(result);
    });
    TurnExecution {
        acceptance,
        completion,
        cancel_flag,
    }
}

fn accept(sender: &mut Option<oneshot::Sender<Result<TurnAcceptance>>>, value: TurnAcceptance) {
    if let Some(sender) = sender.take() {
        let _ = sender.send(Ok(value));
    }
}

async fn execute_owned(
    input: ExecutionInput,
    accepted: &mut Option<oneshot::Sender<Result<TurnAcceptance>>>,
) -> Result<TurnCompletion> {
    let ExecutionInput {
        session,
        layout,
        turn_id,
        workspace,
        request,
        connection,
        shutdown,
        observer,
        new_session,
    } = input;
    let project = workspace.store_key();
    if session
        .read(move |read| read.turn_node(turn_id))
        .await?
        .is_some()
    {
        return Err(ApiError::Conflict(RequestConflict::TurnIdUsed));
    }
    let session_id = request
        .session_id
        .or(new_session)
        .ok_or_else(|| ApiError::BadRequest("explicit SessionId required".into()))?;
    let io = CoordinatorStoreIo::acquire_api(session.clone(), session_id).await?;
    let coordinator = TurnCoordinator::new(io, Arc::new(RandomIdSource));
    if request.session_id.is_some() {
        let record = coordinator
            .io
            .read(move |read| read.session(session_id))
            .await?;
        if record.project_store_key != project {
            return Err(ApiError::Conflict(RequestConflict::BindingMismatch));
        }
    }
    let selected = match &request.agent {
        Some(name) if !name.trim().is_empty() => name.clone(),
        Some(_) => return Err(ApiError::BadRequest("agent name must not be blank".into())),
        None if request.session_id.is_some() => coordinator
            .io
            .read(move |read| read.current_agent(session_id))
            .await?
            .ok_or_else(|| ApiError::BadRequest("root requires explicit agent".into()))?,
        None => {
            return Err(ApiError::BadRequest(
                "new Session requires explicit agent".into(),
            ));
        }
    };
    let loaded = request
        .loaded_profile
        .as_ref()
        .ok_or_else(|| ApiError::BadRequest("frozen configuration required".into()))?;
    if loaded.profile.agent.name != selected {
        return Err(ApiError::BadRequest(
            "loaded agent differs from selection".into(),
        ));
    }
    if let Some(connection) = &connection
        && connection.model().alias != loaded.profile.agent.model
    {
        return Err(ApiError::BadRequest(
            "connection model differs from profile".into(),
        ));
    }
    let (llm, provider): (Arc<dyn lattice_agent::LlmClient>, _) =
        match (request.injected_llm(), &connection) {
            (Some((client, provider)), _) => (client, provider),
            (_, Some(connection)) => (
                Arc::new(super::llm::RealLlmClient(connection.clone())),
                connection.model().provider.clone(),
            ),
            _ => return Err(ApiError::BadRequest("frozen connection required".into())),
        };
    if shutdown.load(Ordering::Acquire) {
        return Err(StoreError::WriteDisabled.into());
    }
    if request.session_id.is_none() {
        let layout = layout.clone();
        tokio::task::spawn_blocking(move || {
            lancet_api::WorkspaceHistory::create(lancet_api::WorkspaceHistoryBacking::for_key(
                layout, project,
            ))
            .map_err(|_| ApiError::StoreUnavailable("workspace history creation failed".into()))
        })
        .await
        .map_err(|_| ApiError::WorkerFailed)??;
    }
    let root = workspace
        .canonical_path()
        .to_str()
        .ok_or_else(|| ApiError::BadRequest("runtime workspace requires UTF-8".into()))?;
    let deps = build_production_turn_deps(ProductionTurnDepsInput {
        request: &request,
        project_root: root,
        project_store_key: project,
        state_layout: layout,
        enabled_tools: &loaded.profile.tools.enabled,
        tls_roots_der: connection.as_ref().map_or(&[], |c| c.tls_roots_der()),
        llm,
        lancet: Some(&loaded.profile.lancet),
    })
    .await?;
    let config = super::live::freeze_config(&request, provider.as_str(), &deps.tool_catalog)?;
    let timeout = config.limits.timeout_secs;
    if shutdown.load(Ordering::Acquire) {
        return Err(StoreError::WriteDisabled.into());
    }
    let result = coordinator
        .io
        .accept(AcceptTurn {
            session: if request.session_id.is_some() {
                AcceptSession::Existing { id: session_id }
            } else {
                AcceptSession::New { id: session_id }
            },
            turn_id,
            project_store_key: project,
            input: request.input,
            config,
            accepted_at: chrono::Utc::now(),
            agent_selection: if request.agent.is_some() {
                AgentSelection::Explicit
            } else {
                AgentSelection::Inherited
            },
        })
        .await;
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            if error.requires_reopen() {
                accept(
                    accepted,
                    TurnAcceptance::ConfirmationUnknown {
                        turn_id,
                        session_id,
                    },
                );
            }
            return Err(error.into());
        }
    };
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout);
    #[cfg(feature = "test-crash-barriers")]
    super::crash_barrier::hit(&coordinator.io, turn_id, "turn_accepted", None, None).await?;
    #[cfg(test)]
    let fail_read = {
        let gate = observer
            .accepted_read_failure
            .lock()
            .expect("accepted read gate")
            .take();
        if let Some((entered, release)) = gate {
            let _ = entered.send(());
            release.await.expect("release accepted read");
            true
        } else {
            false
        }
    };
    let turn = coordinator
        .io
        .read(move |read| {
            #[cfg(test)]
            if fail_read {
                return Err(StoreError::Corruption {
                    table: "test".into(),
                    key: turn_id.to_string(),
                    detail: "注入接受后的读取失败".into(),
                });
            }
            read.turn_detail(session_id, turn_id)
        })
        .await?;
    if matches!(result, AcceptedTurn::Existing(_)) {
        return Err(ApiError::Conflict(RequestConflict::TurnIdUsed));
    }
    accept(accepted, TurnAcceptance::Accepted { turn: turn.clone() });
    observer.emit(TurnEvent::TurnStarted {
        turn: TurnRef {
            session_id,
            turn_id,
        },
        user_node: turn.user_node,
    });
    let detail = super::saga::run_coordinator_saga(
        &coordinator,
        turn_id,
        super::saga::LiveExecution {
            driver: Arc::new(deps.model_driver),
            runtime: Arc::new(deps.runtime),
            events: deps.events,
            observer,
            cancel: request.cancel_flag.expect("cancel carrier initialized"),
            shutdown,
            deadline,
            retry: loaded.profile.llm.retry.clone(),
        },
    )
    .await?;
    Ok(TurnCompletion::Closed(Box::new(detail)))
}

#[cfg(any(test, feature = "test-utils"))]
pub async fn run_turn(
    session: SessionApi,
    layout: StateLayout,
    request: RunTurnRequest,
) -> Result<RunTurnResult> {
    let root = request
        .test_project_root
        .clone()
        .ok_or_else(|| ApiError::BadRequest("explicit test workspace required".into()))?;
    let workspace = tokio::task::spawn_blocking(move || {
        LocalWorkspaceRoot::from_path(std::path::Path::new(&root))
    })
    .await
    .map_err(|_| ApiError::WorkerFailed)??;
    let id = TurnId::parse(&uuid::Uuid::new_v4().to_string()).expect("UUID v4");
    let execution = start_turn(session, layout, id, workspace, request, None);
    execution
        .acceptance
        .await
        .map_err(|_| ApiError::WorkerFailed)??;
    let TurnCompletion::Closed(detail) = execution
        .completion
        .await
        .map_err(|_| ApiError::WorkerFailed)??;
    Ok(RunTurnResult {
        turn: TurnRef {
            session_id: detail.session_id,
            turn_id: id,
        },
        summary: detail.summary,
        usage: detail.usage,
        steps: detail.steps,
    })
}
