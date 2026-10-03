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
use lattice_conversation::{ConversationItem, ProviderOrigin};
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

pub(crate) struct LiveExecution {
    pub driver: Arc<ModelStepDriver>,
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
        .read(move |read| Ok(read.durable_turn(session, turn_id)?.turn.config))
        .await?;
    let origin = match config.provider.as_str() {
        "openai" | "openai_compatible" => ProviderOrigin::OpenAiCompatible,
        "anthropic" => ProviderOrigin::Anthropic,
        "gemini" => ProviderOrigin::Gemini,
        _ => {
            return Err(StoreError::InvalidInput {
                field: "provider",
                detail: "unknown frozen provider".into(),
            }
            .into());
        }
    };
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
        if let Some(cause) = live.stop().or_else(|| {
            (ordinal >= config.limits.max_model_steps).then_some(StopCause::ModelStepLimit)
        }) {
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
            coordinator.io.start_request(key).await?;
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
                provider: origin,
                model: config.model.clone(),
            });
            let cancellation = Arc::new(AtomicBool::new(live.cancel.load(Ordering::Acquire)));
            let request = ModelStepRequest {
                system_prompt: config.system_prompt.clone(),
                history: history.clone(),
                model: config.model.clone(),
                provider: Some(config.provider.clone()),
                tools: tools.clone(),
                temperature: None,
                max_tokens: None,
                cancel: Some(cancellation.clone()),
                deadline: Some(live.deadline),
                session_id: Some(session.to_string()),
            };
            let driver = live.driver.clone();
            let observer = live.observer.clone();
            live.healthy()?;
            let worker = coordinator
                .io
                .start_execution(
                    async move {
                        driver
                            .run(
                                &request,
                                Some(&mut |event| match event {
                                    ModelStepEvent::TokenDelta { content } => {
                                        observer.emit(TurnEvent::TokenDelta { key, content })
                                    }
                                    ModelStepEvent::ReasoningDelta { content } => {
                                        observer.emit(TurnEvent::ReasoningDelta { key, content })
                                    }
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
            let result = match worker {
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
            };
            let mut retry_after = None;
            let stopped = match result {
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
                        origin,
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
                    origin,
                    usage,
                    cause: map_stop(cause),
                    diagnostic: AttemptDiagnostic {
                        provider_error: diagnostic
                            .provider_error
                            .as_deref()
                            .map(bound_provider_diagnostic),
                        invalid_batch: diagnostic.invalid_batch,
                        partial: Vec::new(),
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
                        origin,
                        usage: None,
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
                    origin,
                    usage: None,
                    cause: StopCause::ProviderError { retryable: false },
                    diagnostic: AttemptDiagnostic {
                        provider_error: Some("provider request projection failed".into()),
                        ..Default::default()
                    },
                },
            };
            let retryable = matches!(stopped.cause, StopCause::ProviderError { retryable: true });
            let message = stopped
                .diagnostic
                .provider_error
                .clone()
                .unwrap_or_default();
            let provider_error = matches!(stopped.cause, StopCause::ProviderError { .. });
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
            let wait = Duration::from_millis(delay)
                .max(retry_after.unwrap_or_default())
                .min(live.deadline.saturating_duration_since(Instant::now()));
            if provider_error {
                let will_retry = matches!(stopped, AttemptStopResult::RetryReady { .. });
                live.observer.emit(TurnEvent::LlmRequestError {
                    key,
                    retryable,
                    will_retry,
                    delay_ms: will_retry.then_some(wait.as_millis() as u64),
                    message,
                });
            }
            match stopped {
                AttemptStopResult::Closed(closed) => {
                    return publish_closed(coordinator, &live.observer, closed).await;
                }
                AttemptStopResult::RetryReady { next_attempt } => {
                    live.backoff(wait).await?;
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
        let registered = match coordinator.register(reply).await {
            Ok(calls) => calls,
            Err(CoordinatorIoError::Store(StoreError::InvalidInput { .. })) => {
                let stopped = coordinator
                    .io
                    .stop_attempt(AttemptStopped {
                        key,
                        origin,
                        usage,
                        cause: StopCause::InvalidBatch,
                        diagnostic: AttemptDiagnostic::default(),
                    })
                    .await?;
                let AttemptStopResult::Closed(closed) = stopped else {
                    unreachable!("InvalidBatch is not retryable")
                };
                return publish_closed(coordinator, &live.observer, closed).await;
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
            let StepDetailState::Executing { slots } =
                &detail.steps.last().expect("active Step").state
            else {
                unreachable!()
            };
            let row = slots
                .iter()
                .find(|slot| slot.invocation_id == binding.invocation_id)
                .expect("committed slot");
            let OperatorInvocationState::Terminal { material, .. } = &row.state else {
                unreachable!()
            };
            live.observer.emit(TurnEvent::ToolCallFinished {
                id: binding.invocation_id,
                name: call.name.clone(),
                outcome,
                material: material.clone(),
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
        let closed = coordinator
            .io
            .close_step(CloseStep::Continue {
                key: registered.key,
            })
            .await?;
        publish_closed(coordinator, &live.observer, closed).await?;
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
        match read.node(NodeRef {
            session_id,
            node: current,
        })? {
            Node::UserMessage {
                parent, message, ..
            } => {
                items.push(ConversationItem::Message(message));
                current = parent;
            }
            Node::Step {
                parent, content, ..
            } => {
                items.push(ConversationItem::Step(content));
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
    let turn_id = closed.record.key.turn_id;
    let detail = coordinator
        .io
        .read(move |read| read.turn_detail(session_id, turn_id))
        .await?;
    let step = detail.steps.last().expect("closed Step exists").clone();
    #[cfg(feature = "test-crash-barriers")]
    super::crash_barrier::hit(
        &coordinator.io,
        turn_id,
        "step_closed",
        Some(step.key.ordinal),
        None,
    )
    .await?;
    let turn = TurnRef {
        session_id,
        turn_id,
    };
    #[cfg(feature = "test-crash-barriers")]
    if detail.summary.is_some() {
        super::crash_barrier::schedule(
            "turn_closed",
            serde_json::json!({
                "session_id":session_id,"turn_id":turn_id,"step":step.key.ordinal
            }),
        )
        .await;
    }
    observer.emit(match &detail.summary {
        None => TurnEvent::StepClosed { step },
        Some(summary) => match summary.outcome {
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
        },
    });
    Ok(detail)
}
