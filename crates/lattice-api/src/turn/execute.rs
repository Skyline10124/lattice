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
use lattice_conversation::{Effort, SessionId, TurnId};
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EffortSource {
    Explicit,
    ProfileFile(std::path::PathBuf),
    SessionCopy,
}

impl PreparedModel {
    /// 在建库与接受前完成单次准入与档位选择；同一 Turn 随后沿用这个网关句柄。
    pub(crate) async fn resolve(
        gateway: &Arc<lattice_llm::RequestGateway>,
        profile: &crate::profile::PreparedProfile,
        model: Option<&crate::ModelRef>,
        effort: Option<lattice_conversation::Effort>,
    ) -> Result<Self> {
        let auth = gateway.auth_store();
        let (chosen_model, policy, source) =
            select_model_and_policy(profile, model, effort, Some(&auth))?;
        let turn = gateway.open_turn();
        let resolved = turn
            .prepare(&chosen_model, policy)
            .await
            .map_err(|error| prepare_rejection(&error, &source, Some(&auth)))?;
        Ok(Self {
            target: resolved.target().clone(),
            execution: super::saga::ModelExecution::gateway(turn, &resolved),
        })
    }
}

fn prepare_rejection(
    error: &lattice_llm::PrepareError,
    source: &EffortSource,
    auth: Option<&lattice_llm::auth::AuthStore>,
) -> ApiError {
    let message = match error {
        lattice_llm::PrepareError::UnsupportedEffort { .. } => match source {
            EffortSource::Explicit => format!("{error} (from --effort)"),
            EffortSource::ProfileFile(path) => {
                format!("{error} (from [agent].effort in {})", path.display())
            }
            EffortSource::SessionCopy => format!("{error} (from session profile copy)"),
        },
        _ => error.to_string(),
    };
    ApiError::Configuration(crate::ConfigRejection::new(&message, auth))
}

fn select_model_and_policy(
    profile: &crate::profile::PreparedProfile,
    model: Option<&crate::ModelRef>,
    effort: Option<Effort>,
    auth: Option<&lattice_llm::auth::AuthStore>,
) -> Result<(crate::ModelRef, lattice_llm::EffortPolicy, EffortSource)> {
    let profile_default = profile.configuration.effective.default_effort;
    let base = if profile.inherited {
        let current = &profile.previous.as_ref().expect("inherited copy").current;
        Some((
            crate::ModelRef::new(current.provider.clone(), current.model.clone()),
            current.effort,
        ))
    } else {
        profile
            .configuration
            .effective
            .default_model
            .clone()
            .map(|default_model| (default_model, profile_default))
    };
    let chosen_model = match model {
        Some(model) => model.clone(),
        None => base
            .as_ref()
            .map(|(base_model, _)| base_model.clone())
            .ok_or_else(|| {
                ApiError::Configuration(crate::ConfigRejection::new(
                    "model selection or [agent].model is required",
                    auth,
                ))
            })?,
    };
    let changed = base
        .as_ref()
        .is_none_or(|(base_model, _)| *base_model != chosen_model);
    let base_effort = base
        .as_ref()
        .map_or(profile_default, |(_, base_effort)| *base_effort);
    let profile_source = || {
        if profile.inherited {
            EffortSource::SessionCopy
        } else {
            EffortSource::ProfileFile(
                profile
                    .source_path
                    .clone()
                    .expect("loaded profile source path"),
            )
        }
    };
    let (policy, source) = match effort {
        Some(explicit) => (
            lattice_llm::EffortPolicy::Strict(Some(explicit)),
            EffortSource::Explicit,
        ),
        None if !changed => (
            lattice_llm::EffortPolicy::Strict(base_effort),
            profile_source(),
        ),
        None => (
            lattice_llm::EffortPolicy::Fallback {
                current: base_effort,
                profile_default,
            },
            profile_source(),
        ),
    };
    Ok((chosen_model, policy, source))
}

/// 后台接管与持久接受不同。这个句柄的回执被释放也不撤销后台责任。
pub fn start_turn(
    session: SessionApi,
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
        turn_id,
        workspace,
        mut request,
        prepared_model,
        prepared_profile,
        io,
        shutdown,
        observer,
        new_session,
    } = input;
    let tools = request.tools.take().unwrap_or_else(default_tool_executor);
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
            let (configuration, source_path) = if inherited {
                let copy = previous.as_ref().expect("inherited copy");
                (crate::profile::TurnProfile::from_copy(copy, prompt), None)
            } else {
                let loaded = request
                    .loaded_profile
                    .as_ref()
                    .ok_or_else(|| ApiError::BadRequest("frozen configuration required".into()))?;
                (
                    crate::profile::TurnProfile::from_loaded(loaded),
                    Some(loaded.path.clone()),
                )
            };
            crate::profile::PreparedProfile {
                configuration,
                previous,
                inherited,
                source_path,
            }
        }
    };
    let selected = match &request.agent {
        Some(name) => name.clone(),
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
    let (target, model) = match prepared_model {
        Some(PreparedModel { target, execution }) => (target, execution),
        None => match request.injected_llm() {
            Some((client, protocol, supported)) => {
                let (chosen_model, policy, source) = select_model_and_policy(
                    &profile,
                    request.model.as_ref(),
                    request.effort,
                    None,
                )?;
                let effort = lattice_llm::select_effort(&policy, &supported).map_err(|effort| {
                    prepare_rejection(
                        &lattice_llm::PrepareError::UnsupportedEffort {
                            model: chosen_model.clone(),
                            effort,
                        },
                        &source,
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
    let config = super::live::freeze_config(&profile.configuration, &target, turn_tools.specs())?;
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
            provider: target.model.provider().clone(),
            model: target.model.model().clone(),
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
            retry: profile.configuration.retry(),
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
    use lattice_conversation::Effort::{High, Low, Max, Off};
    use lattice_llm::EffortPolicy;
    use std::path::PathBuf;

    fn profile() -> crate::profile::PreparedProfile {
        let config = FrozenTurnConfig {
            agent_name: crate::AgentName::parse("test").unwrap(),
            provider: crate::ProviderId::parse("profile").unwrap(),
            model: crate::ModelId::parse("default").unwrap(),
            effort: Some(Low),
            system_prompt: None,
            catalog: ToolCatalogSnapshot::new(vec![]).unwrap(),
            limits: TurnExecutionLimits {
                timeout_secs: crate::TurnTimeout::default(),
                max_model_steps: 32,
                max_model_attempts: 1,
            },
        };
        let copy = SessionProfileCopy::for_test(&config);
        crate::profile::PreparedProfile {
            configuration: crate::profile::TurnProfile::from_copy(&copy, None),
            previous: Some(copy),
            inherited: false,
            source_path: Some(PathBuf::from("/workspace/.lattice/agents/test/agent.toml")),
        }
    }

    #[test]
    fn selected_target_uses_profile_or_copy_before_request_override() {
        let agent_toml = PathBuf::from("/workspace/.lattice/agents/test/agent.toml");
        let mut profile = profile();
        let (chosen, policy, source) = select_model_and_policy(&profile, None, None, None).unwrap();
        assert_eq!(
            (chosen, policy, source),
            (
                crate::ModelRef::parse("profile/default").unwrap(),
                EffortPolicy::Strict(Some(Low)),
                EffortSource::ProfileFile(agent_toml.clone()),
            )
        );
        profile.inherited = true;
        profile.source_path = None;
        profile.previous.as_mut().unwrap().current = SessionModelChoice {
            provider: crate::ProviderId::parse("saved").unwrap(),
            model: crate::ModelId::parse("same").unwrap(),
            effort: Some(High),
        };
        for (request, expected_model, changed) in [
            (None, "saved/same", false),
            (Some("saved/same"), "saved/same", false),
            (Some("other/same"), "other/same", true),
            (Some("saved/org/model"), "saved/org/model", true),
        ] {
            let parsed = request.map(|value| crate::ModelRef::parse(value).unwrap());
            let (chosen, policy, source) =
                select_model_and_policy(&profile, parsed.as_ref(), None, None).unwrap();
            let expected_policy = if changed {
                EffortPolicy::Fallback {
                    current: Some(High),
                    profile_default: Some(Low),
                }
            } else {
                EffortPolicy::Strict(Some(High))
            };
            assert_eq!(
                (chosen, policy, source),
                (
                    crate::ModelRef::parse(expected_model).unwrap(),
                    expected_policy,
                    EffortSource::SessionCopy,
                )
            );
            let (_, explicit_policy, explicit_source) =
                select_model_and_policy(&profile, parsed.as_ref(), Some(Max), None).unwrap();
            assert_eq!(
                (explicit_policy, explicit_source),
                (EffortPolicy::Strict(Some(Max)), EffortSource::Explicit)
            );
        }
        profile.inherited = false;
        profile.source_path = Some(agent_toml.clone());
        let (chosen, policy, source) = select_model_and_policy(&profile, None, None, None).unwrap();
        assert_eq!(
            (chosen, policy, source),
            (
                crate::ModelRef::parse("profile/default").unwrap(),
                EffortPolicy::Strict(Some(Low)),
                EffortSource::ProfileFile(agent_toml.clone()),
            )
        );
        profile.configuration.effective.default_model = None;
        assert!(matches!(
            select_model_and_policy(&profile, None, None, None),
            Err(ApiError::Configuration(_))
        ));
        let override_model = crate::ModelRef::parse("chosen/org/model").unwrap();
        let (chosen, policy, _) =
            select_model_and_policy(&profile, Some(&override_model), None, None).unwrap();
        assert_eq!(
            (chosen, policy),
            (
                override_model,
                EffortPolicy::Fallback {
                    current: Some(Low),
                    profile_default: Some(Low),
                },
            )
        );
    }

    #[test]
    fn unsupported_effort_attaches_source_attribution() {
        let agent_toml = PathBuf::from("/workspace/.lattice/agents/coder/agent.toml");
        let unsupported = lattice_llm::PrepareError::UnsupportedEffort {
            model: crate::ModelRef::parse("openai/gpt-5").unwrap(),
            effort: Off,
        };
        let profile_attribution = format!("(from [agent].effort in {})", agent_toml.display());
        for (source, expected, forbidden) in [
            (
                EffortSource::Explicit,
                "(from --effort)",
                &["[agent].effort", "session profile copy"][..],
            ),
            (
                EffortSource::ProfileFile(agent_toml),
                profile_attribution.as_str(),
                &["--effort", "session profile copy"][..],
            ),
            (
                EffortSource::SessionCopy,
                "(from session profile copy)",
                &["--effort", "[agent].effort"][..],
            ),
        ] {
            let ApiError::Configuration(rejection) = prepare_rejection(&unsupported, &source, None)
            else {
                panic!("expected configuration rejection for {source:?}");
            };
            let message = rejection.to_string();
            assert!(
                message.contains(&unsupported.to_string()),
                "missing base UnsupportedEffort message in: {message}"
            );
            assert!(
                message.contains(expected),
                "expected {expected:?} for {source:?}, got: {message}"
            );
            for unexpected in forbidden {
                assert!(
                    !message.contains(unexpected),
                    "unexpected {unexpected:?} for {source:?}, got: {message}"
                );
            }
        }
    }
}
