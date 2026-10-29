// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 单库 live 循环。提交成功才推进执行，关闭完全由会话库的领域事务完成。

use super::{
    coordinator::{CoordinatorIoError, TurnCoordinator},
    observer::ObserverSink,
    types::{TurnEvent, TurnRef},
};
use lancet_protocol::{ModelAttemptKey, TurnId};
use lattice_agent::{
    ModelStepAttempt, ModelStepDriver, ModelStepError, ModelStepEvent, ModelStepRequest,
    ModelStopCause,
};
use lattice_conversation::ConversationItem;
use lattice_session::store::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::{task::JoinHandle, time::Instant};
use tracing::Instrument;

type Result<T> = std::result::Result<T, CoordinatorIoError>;

/// provider 建议等待（Retry-After）的上限。超过它或超过 Turn 剩余时间时立即以 provider
/// 原因结束 Step，不截断后重发：日配额一类的长等待截短重试基本仍会失败。
const MAX_PROVIDER_RETRY_AFTER: Duration = Duration::from_secs(60);

fn retry_after_allowed(wait: Duration, deadline: Instant) -> bool {
    wait <= MAX_PROVIDER_RETRY_AFTER && wait < deadline.saturating_duration_since(Instant::now())
}

pub(crate) enum ModelExecution {
    Gateway {
        turn: lattice_llm::GatewayTurn,
        last_origin: AttemptOrigin,
    },
    Injected {
        driver: Arc<ModelStepDriver>,
        origin: AttemptOrigin,
    },
}

impl ModelExecution {
    pub(crate) fn gateway(
        turn: lattice_llm::GatewayTurn,
        resolved: &lattice_llm::ResolvedRequest,
    ) -> Self {
        Self::Gateway {
            turn,
            last_origin: resolved.origin(),
        }
    }

    async fn resolve(
        &mut self,
        target: &lattice_llm::ModelTarget,
    ) -> (
        AttemptOrigin,
        std::result::Result<Arc<ModelStepDriver>, crate::ConfigRejection>,
    ) {
        match self {
            Self::Gateway { turn, last_origin } => {
                match turn.resolve(target).await {
                    Ok(resolved) => {
                        *last_origin = resolved.origin();
                        (
                            last_origin.clone(),
                            Ok(Arc::new(ModelStepDriver::new(Arc::new(resolved)))),
                        )
                    }
                    Err(error) => {
                        // #331：解析失败尚无本次来源，provider 取自模型引用，协议与 base_url 暂按最近一次成功解析的结果记失败尝试，不发送请求。
                        let origin = AttemptOrigin {
                            provider_id: target.provider.clone(),
                            protocol: last_origin.protocol,
                            base_url: last_origin.base_url.clone(),
                        };
                        (
                            origin,
                            Err(crate::ConfigRejection::new(
                                &error.to_string(),
                                Some(&turn.auth_store()),
                            )),
                        )
                    }
                }
            }
            Self::Injected { driver, origin } => (origin.clone(), Ok(driver.clone())),
        }
    }
}

pub(crate) struct LiveExecution {
    pub model: ModelExecution,
    pub runtime: Arc<lancet_api::LancetRuntime>,
    pub events: tokio::sync::mpsc::UnboundedReceiver<lancet_protocol::ToolEvent>,
    pub observer: Arc<ObserverSink>,
    pub cancel: Arc<AtomicBool>,
    /// 库代故障，不是用户取消。退出时不向失败代写入内存结果。
    pub shutdown: Arc<AtomicBool>,
    pub deadline: Instant,
    pub retry: lattice_profile::LlmRetrySection,
}

impl LiveExecution {
    fn healthy(&self) -> Result<()> {
        if self.shutdown.load(Ordering::Acquire) {
            Err(StoreError::WriteDisabled.into())
        } else {
            Ok(())
        }
    }

    fn stop(&self) -> Option<StopCause> {
        if self.cancel.load(Ordering::Acquire) {
            Some(StopCause::Cancelled)
        } else if Instant::now() >= self.deadline {
            Some(StopCause::Deadline)
        } else {
            None
        }
    }

    /// 不丢弃已启动 worker。故障只请求协作停止，等待实际返回后再释放执行责任。
    async fn join<T>(
        &mut self,
        mut worker: JoinHandle<T>,
        cancellation: Arc<AtomicBool>,
    ) -> Result<T> {
        loop {
            if self.cancel.load(Ordering::Acquire) || self.shutdown.load(Ordering::Acquire) {
                cancellation.store(true, Ordering::Release);
            }
            tokio::select! {
                result = &mut worker => {
                    self.drain_events();
                    self.healthy()?;
                    return Ok(result?);
                }
                Some(event) = self.events.recv() => self.emit_tool(event),
                _ = tokio::time::sleep(Duration::from_millis(10)) => {}
            }
        }
    }

    fn emit_tool(&self, event: lancet_protocol::ToolEvent) {
        if let Some(event) = super::cep::map_cep_tool_event(event) {
            self.observer.emit(event);
        }
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            self.emit_tool(event);
        }
    }

    async fn backoff(&self, delay: Duration) -> Result<()> {
        let until = (Instant::now() + delay).min(self.deadline);
        while Instant::now() < until && self.stop().is_none() {
            self.healthy()?;
            tokio::time::sleep_until(until.min(Instant::now() + Duration::from_millis(10))).await;
        }
        self.healthy()
    }
}

/// 调用者已接受本 Turn 并持有独占 SessionExecutionLock；这里不恢复旧请求。
pub(crate) async fn run_coordinator_saga(
    coordinator: &TurnCoordinator,
    turn_id: TurnId,
    mut live: LiveExecution,
) -> Result<TurnDetail> {
    let session = coordinator.io.session_id();
    let config = coordinator
        .io
        .read(move |read| read.turn_config(session, turn_id))
        .await?;
    let tools = config
        .catalog
        .entries
        .iter()
        .map(|tool| {
            lattice_llm::ToolDefinition::new(
                tool.name.clone(),
                tool.description.clone(),
                tool.input_schema.clone(),
            )
        })
        .collect::<Vec<_>>();
    let mut ordinal = 0;
    loop {
        live.healthy()?;
        if let Some(cause) = live.stop() {
            let closed = coordinator
                .io
                .close_step(CloseStep::StopBetweenRequests { turn_id, cause })
                .await?;
            return publish_closed(coordinator, &live.observer, closed).await;
        }
        let history = coordinator
            .io
            .read(move |read| history(read, session))
            .await?;
        let mut attempt = 0;
        let mut delay = live.retry.initial_delay_ms.min(live.retry.max_delay_ms);
        let reply = loop {
            live.healthy()?;
            if let Some(cause) = live.stop() {
                let closed = coordinator
                    .io
                    .close_step(CloseStep::StopBetweenRequests { turn_id, cause })
                    .await?;
                return publish_closed(coordinator, &live.observer, closed).await;
            }
            let key = ModelAttemptKey {
                turn_id,
                model_step_ordinal: ordinal,
                attempt_ordinal: attempt,
            };
            let target = lattice_llm::ModelTarget::new(&config.provider, &config.model);
            let (origin, driver) = live.model.resolve(&target).await;
            live.healthy()?;
            if let Some(cause) = live.stop() {
                let closed = coordinator
                    .io
                    .close_step(CloseStep::StopBetweenRequests { turn_id, cause })
                    .await?;
                return publish_closed(coordinator, &live.observer, closed).await;
            }
            coordinator.io.start_request(key, origin.clone()).await?;
            #[cfg(feature = "test-crash-barriers")]
            super::crash_barrier::hit(
                &coordinator.io,
                turn_id,
                "request_started",
                Some(ordinal),
                None,
            )
            .await?;
            live.observer.emit(TurnEvent::ModelStarted {
                key,
                origin: origin.clone(),
                model: config.model.clone(),
            });
            let cancellation = Arc::new(AtomicBool::new(live.cancel.load(Ordering::Acquire)));
            let request = ModelStepRequest {
                system_prompt: config.system_prompt.clone(),
                history: history.clone(),
                model: config.model.clone(),
                provider: config.provider.clone(),
                tools: tools.clone(),
                cancel: Some(cancellation.clone()),
                deadline: Some(live.deadline),
                session_id: Some(session.to_string()),
            };
            let observer = live.observer.clone();
            live.healthy()?;
            let result = match driver {
                Ok(driver) => {
                    let worker = coordinator
                        .io
                        .start_execution(
                            async move {
                                driver
                                    .run(
                                        &request,
                                        Some(&mut |event| match event {
                                            ModelStepEvent::TokenDelta { content } => observer
                                                .emit(TurnEvent::TokenDelta { key, content }),
                                            ModelStepEvent::ReasoningDelta { content } => observer
                                                .emit(TurnEvent::ReasoningDelta { key, content }),
                                            ModelStepEvent::Opened { .. } => {}
                                        }),
                                    )
                                    .await
                            }
                            // lattice-llm 的请求、重发与连接日志都落在这个 span 下，可对回具体尝试。
                            .instrument(tracing::info_span!(
                                "model_attempt",
                                session = %session,
                                turn = %turn_id,
                                step = ordinal,
                                attempt
                            )),
                            live.cancel.clone(),
                            live.deadline,
                        )
                        .await?;
                    match worker {
                        Ok(worker) => live.join(worker, cancellation).await?,
                        Err(cause) => Ok(ModelStepAttempt::Stopped {
                            cause: if cause == StopCause::Cancelled {
                                ModelStopCause::Cancelled
                            } else {
                                ModelStopCause::Deadline
                            },
                            usage: None,
                            diagnostic: Default::default(),
                        }),
                    }
                }
                Err(error) => Err(ModelStepError::OpeningFailed {
                    message: lattice_llm::bound_diagnostic(&error.to_string()),
                    retryable: false,
                    retry_after: None,
                }),
            };
            let mut retry_after = None;
            let mut stopped = match result {
                Ok(ModelStepAttempt::Complete {
                    message,
                    usage,
                    finish_reason,
                }) => {
                    live.observer.emit(TurnEvent::ModelFinished {
                        key,
                        usage,
                        finish_reason,
                    });
                    break CompleteReply {
                        key,
                        origin: origin.clone(),
                        message,
                        usage,
                    };
                }
                Ok(ModelStepAttempt::Stopped {
                    cause,
                    usage,
                    diagnostic,
                }) => AttemptStopped {
                    key,
                    origin: origin.clone(),
                    usage,
                    retry_delay_ms: None,
                    cause: map_stop(cause),
                    diagnostic: AttemptDiagnostic {
                        provider_error: diagnostic
                            .provider_error
                            .as_deref()
                            .map(bound_provider_diagnostic),
                        invalid_batch: diagnostic.invalid_batch,
                    },
                },
                Err(ModelStepError::OpeningFailed {
                    message,
                    retryable,
                    retry_after: hint,
                }) => {
                    retry_after = hint;
                    AttemptStopped {
                        key,
                        origin: origin.clone(),
                        usage: None,
                        retry_delay_ms: None,
                        cause: StopCause::ProviderError {
                            retryable: retryable
                                && hint.is_none_or(|wait| retry_after_allowed(wait, live.deadline)),
                        },
                        diagnostic: AttemptDiagnostic {
                            provider_error: Some(bound_provider_diagnostic(&message)),
                            ..Default::default()
                        },
                    }
                }
                Err(ModelStepError::Projection(_)) => AttemptStopped {
                    key,
                    origin: origin.clone(),
                    usage: None,
                    retry_delay_ms: None,
                    cause: StopCause::ProviderError { retryable: false },
                    diagnostic: AttemptDiagnostic {
                        provider_error: Some("provider request projection failed".into()),
                        ..Default::default()
                    },
                },
            };
            // 等待计划随失败尝试提交；事件与实际退避复用同一个毫秒值。
            let retry_delay_ms = Duration::from_millis(delay)
                .max(retry_after.unwrap_or_default())
                .min(live.deadline.saturating_duration_since(Instant::now()))
                .as_millis() as u64;
            if stopped.cause == (StopCause::ProviderError { retryable: true }) {
                stopped.retry_delay_ms = Some(retry_delay_ms);
            }
            let message = stopped
                .diagnostic
                .provider_error
                .clone()
                .unwrap_or_default();
            live.healthy()?;
            let stopped = coordinator.io.stop_attempt(stopped).await?;
            #[cfg(feature = "test-crash-barriers")]
            super::crash_barrier::hit(
                &coordinator.io,
                turn_id,
                "attempt_stopped",
                Some(ordinal),
                None,
            )
            .await?;
            match stopped {
                AttemptStopResult::Closed(closed) => {
                    return publish_closed(coordinator, &live.observer, *closed).await;
                }
                AttemptStopResult::RetryReady { next_attempt } => {
                    live.observer.emit(TurnEvent::LlmRetry {
                        key,
                        delay_ms: retry_delay_ms,
                        notice: Notice {
                            level: NoticeLevel::Warn,
                            text: message,
                        },
                    });
                    live.backoff(Duration::from_millis(retry_delay_ms)).await?;
                    attempt = next_attempt;
                    delay = delay.saturating_mul(2).min(live.retry.max_delay_ms);
                }
            }
        };
        live.healthy()?;
        if reply.message.tool_calls().next().is_none() {
            let closed = coordinator.io.close_step(CloseStep::Finish(reply)).await?;
            return publish_closed(coordinator, &live.observer, closed).await;
        }
        let key = reply.key;
        let usage = reply.usage;
        let origin = reply.origin.clone();
        let registered = match coordinator.register(reply).await {
            Ok(calls) => calls,
            Err(CoordinatorIoError::Store(StoreError::InvalidInput { .. })) => {
                let stopped = coordinator
                    .io
                    .stop_attempt(AttemptStopped {
                        key,
                        origin: origin.clone(),
                        usage,
                        retry_delay_ms: None,
                        cause: StopCause::InvalidBatch,
                        diagnostic: AttemptDiagnostic::default(),
                    })
                    .await?;
                let AttemptStopResult::Closed(closed) = stopped else {
                    unreachable!("InvalidBatch is not retryable")
                };
                return publish_closed(coordinator, &live.observer, *closed).await;
            }
            Err(error) => return Err(error),
        };
        #[cfg(feature = "test-crash-barriers")]
        super::crash_barrier::hit(
            &coordinator.io,
            turn_id,
            "calls_registered",
            Some(ordinal),
            None,
        )
        .await?;
        live.observer.emit(TurnEvent::CallsRegistered {
            key: registered.key,
            calls: registered.bindings.clone(),
        });
        for (binding, (_, call)) in registered
            .bindings
            .iter()
            .zip(registered.reply.tool_calls())
        {
            live.healthy()?;
            if let Some(cause) = live.stop() {
                let stop = if cause == StopCause::Cancelled {
                    DispatchStop::Cancelled
                } else {
                    DispatchStop::Deadline
                };
                let closed = coordinator
                    .io
                    .close_step(CloseStep::StopExecuting {
                        key: registered.key,
                        stop,
                    })
                    .await?;
                return publish_closed(coordinator, &live.observer, closed).await;
            }
            let started = coordinator.io.start_invocation(binding.clone()).await?;
            #[cfg(feature = "test-crash-barriers")]
            super::crash_barrier::hit(
                &coordinator.io,
                turn_id,
                "invocation_started",
                Some(ordinal),
                Some(binding),
            )
            .await?;
            live.healthy()?;
            let runtime = live.runtime.clone();
            let method = call.name.clone();
            let params = call.arguments.clone();
            let cancellation = Arc::new(AtomicBool::new(live.cancel.load(Ordering::Acquire)));
            let call_cancel = cancellation.clone();
            let worker = coordinator
                .io
                .start_execution(
                    async move {
                        runtime
                            .dispatch(started, method, params, Some(call_cancel))
                            .await
                    },
                    live.cancel.clone(),
                    live.deadline,
                )
                .await?;
            let worker = match worker {
                Ok(worker) => worker,
                Err(cause) => {
                    let closed = coordinator
                        .io
                        .close_step(CloseStep::StopExecuting {
                            key: registered.key,
                            stop: if cause == StopCause::Cancelled {
                                DispatchStop::Cancelled
                            } else {
                                DispatchStop::Deadline
                            },
                        })
                        .await?;
                    return publish_closed(coordinator, &live.observer, closed).await;
                }
            };
            live.observer.emit(TurnEvent::ToolCallStarted {
                id: binding.invocation_id,
                name: call.name.clone(),
            });
            let receipt = live.join(worker, cancellation).await?;
            let terminal = prepare_terminal(binding, receipt.result, receipt.evidence)?;
            let outcome = terminal.result().payload.outcome();
            let error_summary = match &terminal.result().payload {
                ResultPayload::Failed(error) => Some(bound_provider_diagnostic(&error.message)),
                _ => None,
            };
            let stop = stops_batch(&terminal.result().payload);
            coordinator
                .io
                .finish_invocation(TerminalInput {
                    invocation_id: binding.invocation_id,
                    terminal,
                })
                .await?;
            #[cfg(feature = "test-crash-barriers")]
            super::crash_barrier::hit(
                &coordinator.io,
                turn_id,
                "invocation_finished",
                Some(ordinal),
                Some(binding),
            )
            .await?;
            let detail = coordinator
                .io
                .read(move |read| read.turn_detail(session, turn_id))
                .await?;
            let row = detail
                .steps
                .last()
                .expect("active Step")
                .slots
                .iter()
                .find(|slot| slot.invocation_id == binding.invocation_id)
                .expect("committed slot");
            live.observer.emit(TurnEvent::ToolCallFinished {
                id: binding.invocation_id,
                name: call.name.clone(),
                outcome,
                material: row.material.clone(),
                error_summary,
            });
            if stop {
                let closed = coordinator
                    .io
                    .close_step(CloseStep::StopExecuting {
                        key: registered.key,
                        stop: DispatchStop::ProcessExit,
                    })
                    .await?;
                return publish_closed(coordinator, &live.observer, closed).await;
            }
        }
        live.healthy()?;
        let close = match live.stop() {
            Some(cause) => CloseStep::StopExecuting {
                key: registered.key,
                stop: if cause == StopCause::Cancelled {
                    DispatchStop::Cancelled
                } else {
                    DispatchStop::Deadline
                },
            },
            None => CloseStep::Continue {
                key: registered.key,
            },
        };
        let closed = coordinator.io.close_step(close).await?;
        let finished = closed.summary.progress.is_closed();
        let detail = publish_closed(coordinator, &live.observer, closed).await?;
        if finished {
            return Ok(detail);
        }
        ordinal += 1;
    }
}

fn map_stop(cause: ModelStopCause) -> StopCause {
    match cause {
        ModelStopCause::ProviderError { retryable } => StopCause::ProviderError { retryable },
        ModelStopCause::Cancelled => StopCause::Cancelled,
        ModelStopCause::Deadline => StopCause::Deadline,
        ModelStopCause::OutputLimit => StopCause::OutputLimit,
        ModelStopCause::InvalidBatch => StopCause::InvalidBatch,
    }
}

fn history(
    read: &SessionRead,
    session_id: lancet_protocol::SessionId,
) -> lattice_session::store::Result<Vec<ConversationItem>> {
    let mut current = read.session(session_id)?.current;
    let mut items = Vec::new();
    while current != NodeId(0) {
        let node = read.node(NodeRef {
            session_id,
            node: current,
        })?;
        let content = node.step_content();
        match node {
            Node::UserMessage {
                parent, message, ..
            } => {
                items.push(ConversationItem::Message(message));
                current = parent;
            }
            Node::Step { parent, .. } => {
                if let Some(content) = content {
                    items.push(ConversationItem::Step(content));
                }
                current = parent;
            }
            Node::Root => {
                return Err(StoreError::Corruption {
                    table: "nodes".into(),
                    key: current.0.to_string(),
                    detail: "nonzero root".into(),
                });
            }
        }
    }
    items.reverse();
    Ok(items)
}

async fn publish_closed(
    coordinator: &TurnCoordinator,
    observer: &ObserverSink,
    closed: ClosedStep,
) -> Result<TurnDetail> {
    let session_id = coordinator.io.session_id();
    let turn_id = closed.step.key.turn_id;
    let detail = coordinator
        .io
        .read(move |read| read.turn_detail(session_id, turn_id))
        .await?;
    let finished = closed.summary.progress.is_closed();
    let step = if finished {
        detail.steps.last().expect("closed Step exists").clone()
    } else {
        closed.step.clone()
    };
    #[cfg(feature = "test-crash-barriers")]
    super::crash_barrier::hit(
        &coordinator.io,
        turn_id,
        "step_closed",
        Some(closed.step.key.ordinal),
        None,
    )
    .await?;
    let turn = TurnRef {
        session_id,
        turn_id,
    };
    #[cfg(feature = "test-crash-barriers")]
    if finished {
        super::crash_barrier::schedule(
            "turn_closed",
            serde_json::json!({
                "session_id":session_id,"turn_id":turn_id,"step":step.key.ordinal
            }),
        )
        .await;
    }
    if finished && closed.next_node.is_some() {
        observer.emit(TurnEvent::StepClosed { step: closed.step });
    }
    observer.emit(if !finished {
        TurnEvent::StepClosed { step }
    } else {
        let summary = &closed.summary;
        match summary.outcome {
            PublicTurnOutcome::Completed => TurnEvent::TurnCompleted {
                turn,
                step,
                summary: summary.clone(),
            },
            PublicTurnOutcome::Failed => TurnEvent::TurnFailed {
                turn,
                step,
                summary: summary.clone(),
            },
            PublicTurnOutcome::Cancelled => TurnEvent::TurnCancelled {
                turn,
                step,
                summary: summary.clone(),
            },
        }
    });
    Ok(detail)
}

#[cfg(test)]
mod resolution_tests {
    use super::*;

    #[tokio::test]
    async fn configuration_failure_uses_latest_successful_resolution_in_the_turn() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(
            directory.path(),
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
        )
        .unwrap();
        let path = directory.path().join("providers.toml");
        let configure = |protocol| {
            std::fs::write(&path, format!(
            "[providers.local]\nprotocol='{protocol}'\nbase_url='http://localhost:9/v1'\nauth='none'\n"
        )).unwrap()
        };
        configure("chat-completions");
        let gateway = lattice_llm::RequestGateway::new(directory.path().into(), vec![]).unwrap();
        let turn = gateway.open_turn();
        let target = lattice_llm::ModelTarget::new("local", "model");
        let initial = turn.resolve(&target).await.unwrap();
        let mut execution = ModelExecution::gateway(turn, &initial);
        configure("generate-content");
        let (latest, ready) = execution.resolve(&target).await;
        assert!(ready.is_ok());
        assert_eq!(latest.protocol, AttemptProtocol::GenerateContent);
        std::fs::write(path, "invalid configuration").unwrap();
        let (failed, request) = execution.resolve(&target).await;
        assert!(
            request
                .err()
                .unwrap()
                .to_string()
                .contains("configuration error")
        );
        assert_eq!(failed, latest);
    }
}
