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
use futures::FutureExt;
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
    pub prepared_model: Option<PreparedModel>,
    pub prepared_profile: Option<crate::profile::PreparedProfile>,
    pub io: Option<CoordinatorStoreIo>,
    pub shutdown: Arc<AtomicBool>,
    pub observer: Arc<ObserverSink>,
    /// 新会话身份由提交方在执行开始前给定。
    pub new_session: Option<SessionId>,
}

pub(crate) struct PreparedModel {
    target: lattice_llm::ModelTarget,
    execution: super::saga::ModelExecution,
    gateway: Arc<lattice_llm::RequestGateway>,
}

impl PreparedModel {
    /// 在建库与接受前完成解析；同一 Turn 随后沿用这个网关句柄。
    pub(crate) async fn resolve(
        gateway: &Arc<lattice_llm::RequestGateway>,
        profile: &crate::profile::PreparedProfile,
        model: Option<&str>,
        effort: Option<lattice_conversation::Effort>,
    ) -> Result<Self> {
        let auth = gateway.auth_store();
        let (mut target, changed) = selected_target(profile, model, Some(&auth))?;
        let turn = gateway.open_turn();
        if let Some(effort) = effort {
            target.effort = Some(effort);
        } else if changed {
            let supported = turn.supported_efforts(&target).await.map_err(|error| {
                ApiError::Configuration(crate::ConfigRejection::new(
                    &error.to_string(),
                    Some(&auth),
                ))
            })?;
            target.effort = [
                target.effort,
                profile.configuration.effective.default_effort,
            ]
            .into_iter()
            .flatten()
            .find(|effort| supported.contains(effort));
        }
        let resolved = turn.resolve(&target).await.map_err(|error| {
            ApiError::Configuration(crate::ConfigRejection::new(&error.to_string(), Some(&auth)))
        })?;
        Ok(Self {
            target,
            execution: super::saga::ModelExecution::gateway(turn, &resolved),
            gateway: gateway.clone(),
        })
    }
}

fn selected_target(
    profile: &crate::profile::PreparedProfile,
    model: Option<&str>,
    auth: Option<&lattice_llm::auth::AuthStore>,
) -> Result<(lattice_llm::ModelTarget, bool)> {
    let base = if profile.inherited {
        let current = &profile.previous.as_ref().expect("inherited copy").current;
        let mut target = lattice_llm::ModelTarget::new(&current.provider, &current.model);
        target.effort = current.effort;
        Some(target)
    } else {
        profile
            .configuration
            .effective
            .default_model
            .as_deref()
            .map(|model| model_target(model, auth))
            .transpose()?
            .map(|mut target| {
                target.effort = profile.configuration.effective.default_effort;
                target
            })
    };
    let mut target = match model {
        Some(model) => model_target(model, auth)?,
        None => base.clone().ok_or_else(|| {
            ApiError::Configuration(crate::ConfigRejection::new(
                "model selection or [agent].model is required",
                auth,
            ))
        })?,
    };
    let changed = base
        .as_ref()
        .is_none_or(|base| base.provider != target.provider || base.model != target.model);
    target.effort = base.map_or(profile.configuration.effective.default_effort, |base| {
        base.effort
    });
    Ok((target, changed))
}

fn model_target(
    model: &str,
    auth: Option<&lattice_llm::auth::AuthStore>,
) -> Result<lattice_llm::ModelTarget> {
    let reference = lattice_llm::ModelRef::parse(model).map_err(|error| {
        ApiError::Configuration(crate::ConfigRejection::new(&error.to_string(), auth))
    })?;
    Ok(lattice_llm::ModelTarget::new(
        reference.provider(),
        reference.model(),
    ))
}

/// 后台接管与持久接受不同。这个句柄的回执被释放也不撤销后台责任。
pub fn start_turn(
    session: SessionApi,
    layout: StateLayout,
    turn_id: TurnId,
    workspace: LocalWorkspaceRoot,
    request: RunTurnRequest,
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
        prepared_model: None,
        prepared_profile: None,
        io: None,
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
        let turn_id = input.turn_id;
        let mut accepted_session = None;
        let result = std::panic::AssertUnwindSafe(execute_owned(
            input,
            &mut accepted,
            &mut accepted_session,
        ))
        .catch_unwind()
        .await
        .unwrap_or(Err(ApiError::WorkerFailed));
        if let (Some(sender), Err(error)) = (accepted.take(), &result) {
            let reply = match accepted_session {
                Some(session_id) => Ok(TurnAcceptance::ConfirmationUnknown {
                    turn_id,
                    session_id,
                }),
                None => Err(error.clone()),
            };
            let _ = sender.send(reply);
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
    accepted_session: &mut Option<SessionId>,
) -> Result<TurnCompletion> {
    let ExecutionInput {
        session,
        layout,
        turn_id,
        workspace,
        request,
        prepared_model,
        prepared_profile,
        io,
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
    let io = match io {
        Some(io) => io,
        None => CoordinatorStoreIo::acquire_api(session.clone(), session_id).await?,
    };
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
    let profile = match prepared_profile {
        Some(profile) => profile,
        None => {
            let snapshot = if request.session_id.is_some() {
                Some(
                    coordinator
                        .io
                        .read(move |read| {
                            let copy = read.profile_copy(session_id)?;
                            let prompt = read
                                .stored_config(&copy.prompt_config_digest)?
                                .system_prompt;
                            Ok((copy, prompt))
                        })
                        .await?,
                )
            } else {
                None
            };
            let (previous, prompt) =
                snapshot.map_or((None, None), |(copy, prompt)| (Some(copy), prompt));
            let inherited = request.agent.is_none() && previous.is_some();
            let configuration = if inherited {
                let copy = previous.as_ref().expect("inherited copy");
                crate::profile::TurnProfile::from_copy(copy, prompt)
            } else {
                crate::profile::TurnProfile::from_loaded(
                    request.loaded_profile.as_ref().ok_or_else(|| {
                        ApiError::BadRequest("frozen configuration required".into())
                    })?,
                    &workspace,
                )?
            };
            crate::profile::PreparedProfile {
                configuration,
                previous,
                inherited,
            }
        }
    };
    let selected = match &request.agent {
        Some(name) if !name.trim().is_empty() => name.clone(),
        Some(_) => return Err(ApiError::BadRequest("agent name must not be blank".into())),
        None if request.session_id.is_some() => profile.configuration.effective.agent_name.clone(),
        None => {
            return Err(ApiError::BadRequest(
                "new Session requires explicit agent".into(),
            ));
        }
    };
    if profile.configuration.effective.agent_name != selected {
        return Err(ApiError::BadRequest(
            "loaded agent differs from selection".into(),
        ));
    }
    let (target, model, gateway) = match prepared_model {
        Some(PreparedModel {
            target,
            execution,
            gateway,
        }) => (target, execution, Some(gateway)),
        None => match request.injected_llm() {
            Some((client, protocol)) => {
                let (mut target, _) = selected_target(&profile, request.model.as_deref(), None)?;
                if request.effort.is_some() {
                    target.effort = request.effort;
                }
                let origin = lattice_session::store::AttemptOrigin {
                    provider_id: target.provider.clone(),
                    protocol,
                };
                (
                    target,
                    super::saga::ModelExecution::Injected {
                        driver: Arc::new(lattice_agent::ModelStepDriver::new(client)),
                        origin,
                    },
                    None,
                )
            }
            None => {
                return Err(ApiError::BadRequest("prepared model required".into()));
            }
        },
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
    let lancet = profile.configuration.lancet();
    let deps = build_production_turn_deps(ProductionTurnDepsInput {
        capability_admission: profile.configuration.capability_admission(),
        project_root: root,
        project_store_key: project,
        state_layout: layout,
        enabled_tools: &profile.configuration.effective.enabled_tools,
        gateway: gateway.clone(),
        lancet: Some(&lancet),
    })
    .await?;
    let config = super::live::freeze_config(&profile.configuration, &target, &deps.tool_catalog)?;
    let expected_profile_digest = profile
        .previous
        .as_ref()
        .map(SessionProfileCopy::digest)
        .transpose()?;
    let profile_copy = SessionProfileCopy {
        profile: profile.configuration.effective.clone(),
        prompt_config_digest: if profile.inherited {
            profile
                .previous
                .as_ref()
                .expect("inherited copy")
                .prompt_config_digest
        } else {
            config.digest()?
        },
        current: SessionModelChoice {
            provider: target.provider.clone(),
            model: target.model.clone(),
            effort: target.effort,
        },
    };
    let timeout = config.limits.timeout_secs;
    if shutdown.load(Ordering::Acquire) {
        return Err(StoreError::WriteDisabled.into());
    }
    #[cfg(test)]
    {
        let gate = observer
            .before_accept
            .lock()
            .expect("before accept gate")
            .take();
        if let Some((entered, release)) = gate {
            let _ = entered.send(());
            release.await.expect("release accept");
        }
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
            profile_copy,
            expected_profile_digest,
        })
        .await;
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            let error = ApiError::from(error);
            if error.outcome_unknown() {
                accept(
                    accepted,
                    TurnAcceptance::ConfirmationUnknown {
                        turn_id,
                        session_id,
                    },
                );
            }
            return Err(error);
        }
    };
    if matches!(result, AcceptedTurn::Existing(_)) {
        return Err(ApiError::Conflict(RequestConflict::TurnIdUsed));
    }
    *accepted_session = Some(session_id);
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
            model,
            runtime: Arc::new(deps.runtime),
            events: deps.events,
            observer,
            cancel: request.cancel_flag.expect("cancel carrier initialized"),
            shutdown,
            deadline,
            retry: profile.configuration.retry(),
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
    let execution = start_turn(session, layout, id, workspace, request);
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
        usage: *detail.usage,
        steps: detail.steps,
    })
}
