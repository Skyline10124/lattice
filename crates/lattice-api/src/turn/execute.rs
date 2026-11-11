// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::{
    coordinator::{CoordinatorStoreIo, RandomIdSource, TurnCoordinator},
    observer::ObserverSink,
    tools::{TurnTools, default_tool_executor},
    types::*,
};
use crate::{
    root::LocalWorkspaceRoot,
    session::{ApiError, RequestConflict, Result, SessionApi},
};
use futures::FutureExt;
use lattice_conversation::{SessionId, TurnId};
use lattice_session::store::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::oneshot;

pub const STREAM_IDLE_TIMEOUT_SECS: u64 = 300;

pub(crate) struct ExecutionInput {
    pub session: SessionApi,
    pub turn_id: TurnId,
    pub workspace: LocalWorkspaceRoot,
    pub request: RunTurnRequest,
    pub prepared_model: Option<PreparedModel>,
    pub prepared_settings: Option<(SessionSettings, SettingsVersion)>,
    pub io: Option<CoordinatorStoreIo>,
    pub shutdown: Arc<AtomicBool>,
    pub observer: Arc<ObserverSink>,
}

pub(crate) struct PreparedModel {
    target: lattice_llm::ModelTarget,
    execution: super::saga::ModelExecution,
}

impl PreparedModel {
    /// 在接受前按会话设置完成单次准入与严格档位校验；同一 Turn 随后沿用这个网关句柄。
    pub(crate) async fn resolve(
        gateway: &Arc<lattice_llm::RequestGateway>,
        settings: &SessionSettings,
    ) -> Result<Self> {
        let auth = gateway.auth_store();
        let chosen_model = require_settings_model(settings, Some(&auth))?;
        let turn = gateway.open_turn();
        let resolved = turn
            .prepare(
                &chosen_model,
                lattice_llm::EffortPolicy::Strict(settings.effort),
            )
            .await
            .map_err(|error| prepare_rejection(&error, Some(&auth)))?;
        Ok(Self {
            target: resolved.target().clone(),
            execution: super::saga::ModelExecution::gateway(turn, &resolved),
        })
    }
}

pub(crate) fn require_settings_model(
    settings: &SessionSettings,
    auth: Option<&lattice_llm::auth::AuthStore>,
) -> Result<crate::ModelRef> {
    settings.model.clone().ok_or_else(|| {
        ApiError::Configuration(crate::ConfigRejection::new(
            "session has no model selected; choose one with /model or --model",
            auth,
        ))
    })
}

fn prepare_rejection(
    error: &lattice_llm::PrepareError,
    auth: Option<&lattice_llm::auth::AuthStore>,
) -> ApiError {
    let message = crate::configure::EffortSource::SessionSettings.format_unsupported(error);
    ApiError::Configuration(crate::ConfigRejection::new(&message, auth))
}

/// 后台接管与持久接受不同。这个句柄的回执被释放也不撤销后台责任。
pub fn start_turn(
    session: SessionApi,
    turn_id: TurnId,
    workspace: LocalWorkspaceRoot,
    request: RunTurnRequest,
) -> TurnExecution {
    start_execution(ExecutionInput {
        session,
        turn_id,
        workspace,
        request,
        prepared_model: None,
        prepared_settings: None,
        io: None,
        shutdown: Arc::default(),
        observer: Arc::default(),
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
        turn_id,
        workspace,
        mut request,
        prepared_model,
        prepared_settings,
        io,
        shutdown,
        observer,
    } = input;
    let tools = request.tools.take().unwrap_or_else(default_tool_executor);
    let project = workspace.project_path();
    if session
        .read(move |read| read.turn_node(turn_id))
        .await?
        .is_some()
    {
        return Err(ApiError::Conflict(RequestConflict::TurnIdUsed));
    }
    let session_id = request.session_id;
    let io = match io {
        Some(io) => io,
        None => CoordinatorStoreIo::acquire_api(session.clone(), session_id).await?,
    };
    let coordinator = TurnCoordinator::new(io, Arc::new(RandomIdSource));
    let (settings, settings_version) = match prepared_settings {
        Some(prepared) => prepared,
        None => {
            let expected = request.expected_settings;
            let check_project = project.clone();
            let (record, settings) = coordinator
                .io
                .read(move |read| {
                    let record = read.session(session_id)?;
                    let settings = read.session_settings(session_id)?;
                    Ok((record, settings))
                })
                .await?;
            if record.project_path != check_project {
                return Err(ApiError::Conflict(RequestConflict::BindingMismatch));
            }
            let version = settings.version()?;
            if let Some(expected) = expected
                && expected != version
            {
                return Err(ApiError::Conflict(RequestConflict::SettingsChanged));
            }
            (settings, version)
        }
    };
    let (target, model) = match prepared_model {
        Some(PreparedModel { target, execution }) => (target, execution),
        None => match request.injected_llm() {
            Some((client, protocol, supported)) => {
                let chosen_model = require_settings_model(&settings, None)?;
                let policy = lattice_llm::EffortPolicy::Strict(settings.effort);
                let effort = lattice_llm::select_effort(&policy, &supported).map_err(|effort| {
                    prepare_rejection(
                        &lattice_llm::PrepareError::UnsupportedEffort {
                            model: chosen_model.clone(),
                            effort,
                        },
                        None,
                    )
                })?;
                let mut target = lattice_llm::ModelTarget::new(chosen_model);
                target.effort = effort;
                let origin = lattice_session::store::AttemptOrigin {
                    provider_id: target.model.provider().clone(),
                    protocol,
                };
                (
                    target,
                    super::saga::ModelExecution::Injected {
                        driver: Arc::new(lattice_agent::ModelStepDriver::new(client)),
                        origin,
                    },
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
    let turn_tools: Arc<dyn TurnTools> = Arc::from(tools.open_turn(session_id, &workspace).await?);
    let config = super::live::freeze_config(&settings, &target, turn_tools.specs())?;
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
            session_id,
            turn_id,
            project_path: project,
            input: request.input,
            config,
            accepted_at: chrono::Utc::now(),
            settings_version,
        })
        .await;
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            let error = match ApiError::from(error) {
                ApiError::Store(store_error) => match store_error.as_ref() {
                    StoreError::Conflict { .. } => ApiError::Conflict(RequestConflict::TurnIdUsed),
                    StoreError::BindingMismatch { .. } => {
                        ApiError::Conflict(RequestConflict::BindingMismatch)
                    }
                    StoreError::SettingsChanged { .. } => {
                        ApiError::Conflict(RequestConflict::SettingsChanged)
                    }
                    _ => ApiError::Store(store_error),
                },
                other => other,
            };
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
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout.as_secs());
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
            tools: turn_tools,
            observer,
            cancel: request.cancel_flag.expect("cancel carrier initialized"),
            shutdown,
            deadline,
            retry: settings.profile.retry,
        },
    )
    .await?;
    Ok(TurnCompletion::Closed(Box::new(detail)))
}

#[cfg(any(test, feature = "test-utils"))]
pub async fn run_turn(
    session: SessionApi,
    workspace: LocalWorkspaceRoot,
    request: RunTurnRequest,
) -> Result<RunTurnResult> {
    let id = TurnId::parse(&uuid::Uuid::new_v4().to_string()).expect("UUID v4");
    let execution = start_turn(session, id, workspace, request);
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

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_conversation::Effort::Off;

    #[test]
    fn admission_requires_model_and_attributes_unsupported_effort_to_session_settings() {
        let modeless = SessionSettings {
            profile: EffectiveProfile {
                agent_name: crate::AgentName::parse("test").unwrap(),
                description: None,
                default_model: None,
                default_effort: None,
                timeout_secs: crate::TurnTimeout::default(),
                retry: crate::RetryPolicy::default(),
            },
            system_prompt: None,
            model: None,
            effort: None,
        };
        let Err(ApiError::Configuration(missing_model)) = require_settings_model(&modeless, None)
        else {
            panic!("模型为空的会话设置应在准入时返回配置拒绝");
        };
        let missing_text = missing_model.to_string();
        assert!(
            missing_text.contains("/model") && missing_text.contains("--model"),
            "缺少模型的拒绝应提示 /model 与 --model，实际为 {missing_text:?}"
        );

        let unsupported = lattice_llm::PrepareError::UnsupportedEffort {
            model: crate::ModelRef::parse("openai/gpt-5").unwrap(),
            effort: Off,
        };
        let ApiError::Configuration(rejection) = prepare_rejection(&unsupported, None) else {
            panic!("预期配置拒绝");
        };
        let message = rejection.to_string();
        assert!(
            message.contains(&unsupported.to_string())
                && message.contains("(from session settings)"),
            "提交准入的档位拒绝必须归因为会话设置: {message}"
        );
    }
}
