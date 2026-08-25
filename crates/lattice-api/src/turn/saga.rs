// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Production TurnCoordinator live saga (#139-B Commit 6).
//!
//! Drives one-attempt [`ModelStepDriver`] calls under durable journal stages:
//! retries, binding, reservation, sequential Tool dispatch, ResultDraft,
//! terminal/evidence, StepClosing, and Live TurnClosing/finalization.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use lancet_api::{ExecutionReceipt, WorkspaceHistory};
use lancet_protocol::{
    BindingIdentity, CepError, CepErrorFamily, InvocationBinding, ModelAttemptKey,
    ModelMessageAddress, PlanKey, SessionId, ToolCallAddress, TurnId, arguments_digest,
    canonical_json,
};
use lattice_agent::{
    ModelStepAttempt, ModelStepError, ModelStepEvent, ModelStepRequest, ModelStepStatus,
};
use lattice_conversation::{
    BoundToolCall, MessagePart, MessageRole, MessageSemantic, NativeMessage, ToolResultOutcome,
    UnboundToolCall, semantic_digest,
};
use lattice_llm::ToolDefinition;
use lattice_session::pending::{
    BindingPlan, ClosedBinding, ClosureOrigin, DraftClassification, DraftPublication,
    MAX_MODEL_STEPS, MaterialAvailability, PendingStage, PlannedBinding, ResultDraft,
    StepClosedKind, StepClosingState, ToolContentError, TurnCancellation, TurnClosingFacts,
    TurnCommitCandidate, TurnFailure, TurnFinishReason, TurnInterruption, TurnOutcome,
    TurnUsageAggregate, UnavailableMaterialReason, WorkspaceHistoryCheckpoint,
};
use lattice_session::{FlockGuard, TurnCommit};
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::cep::{ProductionTurnDeps, map_cep_tool_event};
use super::coordinator::{CoordinatorIoError, TurnCoordinator};

use super::finalize::{FinalizeBudget, FinalizeFailure};
use super::live::{PreparedTurn, TURN_EVENT_CHANNEL_CAPACITY};
use super::observer::{CepOrderedEmitter, ObserverSink};
use super::types::{RunTurnRequest, TurnEvent};
use crate::session::ApiError;

const CEP_EVENT_DRAIN_GRACE: Duration = Duration::from_millis(50);
const BOUNDED_ERROR_CONTENT_BYTES: usize = 4_096;

#[cfg(feature = "test-crash-barriers")]
async fn hit_turn_crash_barrier(
    coordinator: &TurnCoordinator,
    session_id: &str,
    barrier: &str,
) -> Result<(), ApiError> {
    super::crash_barrier::hit(coordinator, session_id, barrier)
        .await
        .map_err(map_io)
}

#[cfg(not(feature = "test-crash-barriers"))]
async fn hit_turn_crash_barrier(
    _coordinator: &TurnCoordinator,
    _session_id: &str,
    _barrier: &str,
) -> Result<(), ApiError> {
    Ok(())
}
/// Drive the durable live Turn after `TurnStarted` is published.
pub(super) async fn run_coordinator_saga(
    prepared: PreparedTurn,
    request: RunTurnRequest,
    turn_id: TurnId,
    deps: ProductionTurnDeps,
    coordinator: Arc<TurnCoordinator>,
    execution_lock: Arc<FlockGuard>,
    mut observer: ObserverSink,
) {
    let ProductionTurnDeps {
        model_driver,
        tool_catalog,
        mut runtime,
        cep_client,
        workspace_history,
        ..
    } = deps;
    let tool_definitions = tool_catalog
        .iter()
        .cloned()
        .map(|tool| ToolDefinition::new(tool.name, tool.description, tool.parameters))
        .collect::<Vec<_>>();
    let session_id = prepared.session_id.clone();
    let session_proto = SessionId::parse(&session_id).expect("canonical SessionId");
    let turn_deadline = Instant::now()
        + Duration::from_secs(
            request
                .loaded_profile
                .as_ref()
                .map(|p| p.profile.llm.timeout_secs.unwrap_or(300))
                .unwrap_or(300),
        );
    let max_attempts = request
        .loaded_profile
        .as_ref()
        .map(|p| p.profile.llm.retry.max_retries.saturating_add(1).max(1))
        .unwrap_or(1);

    let (agent_tx, mut agent_rx) = mpsc::channel::<TurnEvent>(TURN_EVENT_CHANNEL_CAPACITY);
    let cep_forward_client = cep_client.clone();
    let cep_agent_tx = agent_tx.clone();
    let cep_forwarder = tokio::spawn(async move {
        while let Some(event) = cep_forward_client.next_event().await {
            if let Some(agent_event) = map_cep_tool_event(event)
                && cep_agent_tx.send(agent_event).await.is_err()
            {
                break;
            }
        }
    });

    let mut ordered_events = CepOrderedEmitter::default();
    let mut history = prepared.transcript.messages.clone();
    history.push(user_message(&request.input));

    let mut attempt_usages: Vec<Option<lattice_conversation::TokenUsage>> = Vec::new();
    let mut candidate_messages: Vec<NativeMessage<BoundToolCall>> =
        vec![user_message(&request.input)];
    let mut model_step: u32 = 0;
    let mut awaiting_model_continuation = false;
    let mut terminal_reason: Option<String> = None;
    let mut turn_outcome = TurnOutcome::Completed {
        finish_reason: TurnFinishReason::Stop,
    };

    macro_rules! crash_barrier {
        ($label:lifetime, $name:expr) => {
            if let Err(error) = hit_turn_crash_barrier(&coordinator, &session_id, $name).await {
                break $label Err(error);
            }
        };
    }
    let saga_result: Result<(), ApiError> = 'saga: loop {
        if model_step >= MAX_MODEL_STEPS {
            turn_outcome = TurnOutcome::Failed {
                reason: TurnFailure::ModelStepLimitExceeded,
            };
            terminal_reason = Some("model step limit exceeded".into());
            if !awaiting_model_continuation {
                let recorded_at = chrono::Utc::now().timestamp();
                if let Err(err) = coordinator
                    .io
                    .advance_turn(
                        session_id.clone(),
                        PendingStage::StepClosed {
                            kind: StepClosedKind::FailTurn {
                                reason: TurnFailure::ModelStepLimitExceeded,
                            },
                            messages: Vec::new(),
                        },
                        recorded_at,
                        Arc::clone(&execution_lock),
                    )
                    .await
                {
                    break 'saga Err(map_io(err));
                }
            }
            break 'saga Ok(());
        }

        let mut attempt: u32 = 0;
        let complete_message = loop {
            if Instant::now() >= turn_deadline {
                turn_outcome = TurnOutcome::Interrupted {
                    reason: TurnInterruption::ModelAttempt(
                        lattice_conversation::AttemptInterruption::Timeout,
                    ),
                };
                terminal_reason = Some("timeout".into());
                if awaiting_model_continuation {
                    break 'saga Ok(());
                }
                let recorded_at = chrono::Utc::now().timestamp();
                if let Err(err) = coordinator
                    .io
                    .advance_turn(
                        session_id.clone(),
                        PendingStage::StepClosed {
                            kind: StepClosedKind::InterruptTurn {
                                reason: TurnInterruption::ModelAttempt(
                                    lattice_conversation::AttemptInterruption::Timeout,
                                ),
                            },
                            messages: Vec::new(),
                        },
                        recorded_at,
                        Arc::clone(&execution_lock),
                    )
                    .await
                {
                    break 'saga Err(map_io(err));
                }
                break 'saga Ok(());
            }

            if is_cancelled(request.cancel_flag.as_ref()) {
                turn_outcome = TurnOutcome::Cancelled {
                    reason: TurnCancellation::UserCancelled,
                };
                terminal_reason = Some("cancelled by caller".into());
                if awaiting_model_continuation {
                    break 'saga Ok(());
                }
                let recorded_at = chrono::Utc::now().timestamp();
                if let Err(err) = coordinator
                    .io
                    .advance_turn(
                        session_id.clone(),
                        PendingStage::StepClosed {
                            kind: StepClosedKind::CancelTurn {
                                reason: TurnCancellation::UserCancelled,
                            },
                            messages: Vec::new(),
                        },
                        recorded_at,
                        Arc::clone(&execution_lock),
                    )
                    .await
                {
                    break 'saga Err(map_io(err));
                }
                break 'saga Ok(());
            }

            let attempt_key = ModelAttemptKey {
                turn_id,
                model_step_ordinal: model_step,
                attempt_ordinal: attempt,
            };
            let recorded_at = chrono::Utc::now().timestamp();
            if let Err(err) = coordinator
                .io
                .advance_turn(
                    session_id.clone(),
                    PendingStage::ModelAttemptStarted { key: attempt_key },
                    recorded_at,
                    Arc::clone(&execution_lock),
                )
                .await
            {
                break 'saga Err(map_io(err));
            }
            crash_barrier!('saga, "model_attempt_started");
            awaiting_model_continuation = false;

            observer
                .emit(TurnEvent::ModelStarted {
                    provider: Some(prepared.provider.clone()).filter(|p| p != "unknown"),
                    model: prepared.model.clone(),
                })
                .await;

            let (step_tx, mut step_rx) =
                mpsc::channel::<ModelStepEvent>(TURN_EVENT_CHANNEL_CAPACITY);
            let forward_tx = agent_tx.clone();
            let forwarder = tokio::spawn(async move {
                while let Some(event) = step_rx.recv().await {
                    if let Some(mapped) = map_step_event(event)
                        && forward_tx.send(mapped).await.is_err()
                    {
                        break;
                    }
                }
            });

            let step_request = ModelStepRequest {
                system_prompt: prepared.system_prompt.clone(),
                history: history.clone(),
                model: prepared.model.clone(),
                provider: Some(prepared.provider.clone()).filter(|p| p != "unknown"),
                tools: tool_definitions.clone(),
                temperature: None,
                max_tokens: None,
                cancel: request.cancel_flag.clone(),
                deadline: Some(turn_deadline),
            };

            let attempt_result = model_driver.run(&step_request, Some(&step_tx)).await;
            drop(step_tx);
            let _ = forwarder.await;
            drain_ready_events(&mut agent_rx, &mut ordered_events, &mut observer).await;

            match attempt_result {
                Err(ModelStepError::OpeningFailed { message, retryable }) => {
                    attempt_usages.push(None);
                    let recorded_at = chrono::Utc::now().timestamp();
                    let will_retry = retryable && attempt + 1 < max_attempts;
                    observer
                        .emit(TurnEvent::LlmRequestError {
                            provider: Some(prepared.provider.clone()).filter(|p| p != "unknown"),
                            model: prepared.model.clone(),
                            attempt: attempt + 1,
                            max_attempts,
                            error_kind: "opening_failed".into(),
                            retryable,
                            will_retry,
                            delay_ms: None,
                            status_code: None,
                            message: message.clone(),
                        })
                        .await;
                    if let Err(err) = coordinator
                        .io
                        .advance_turn(
                            session_id.clone(),
                            PendingStage::ModelAttemptFailed {
                                key: attempt_key,
                                retryable,
                                usage: None,
                            },
                            recorded_at,
                            Arc::clone(&execution_lock),
                        )
                        .await
                    {
                        break 'saga Err(map_io(err));
                    }
                    if will_retry {
                        attempt += 1;
                        continue;
                    }
                    turn_outcome = TurnOutcome::Failed {
                        reason: TurnFailure::ModelRequestFailed,
                    };
                    terminal_reason = Some(format!("LLM error: {message}"));
                    let recorded_at = chrono::Utc::now().timestamp();
                    if let Err(err) = coordinator
                        .io
                        .advance_turn(
                            session_id.clone(),
                            PendingStage::StepClosed {
                                kind: StepClosedKind::FailTurn {
                                    reason: TurnFailure::ModelRequestFailed,
                                },
                                messages: Vec::new(),
                            },
                            recorded_at,
                            Arc::clone(&execution_lock),
                        )
                        .await
                    {
                        break 'saga Err(map_io(err));
                    }
                    break 'saga Ok(());
                }
                Err(ModelStepError::Projection(error)) => {
                    break 'saga Err(ApiError::Internal(format!(
                        "provider request projection failed: {error}"
                    )));
                }
                Ok(step) => {
                    attempt_usages.push(step.usage);
                    match handle_attempt_outcome(
                        &step,
                        attempt_key,
                        &session_id,
                        &coordinator,
                        &execution_lock,
                        &mut observer,
                    )
                    .await
                    {
                        AttemptHandled::TurnClosed { outcome, reason } => {
                            turn_outcome = outcome;
                            terminal_reason = reason;
                            break 'saga Ok(());
                        }
                        AttemptHandled::Complete(message) => break message,
                        AttemptHandled::Err(err) => break 'saga Err(err),
                    }
                }
            }
        };

        observer
            .emit(TurnEvent::ModelFinished {
                finish_reason: "stop".into(),
                usage: attempt_usages.last().copied().flatten().map(Into::into),
            })
            .await;

        let tool_calls: Vec<(usize, UnboundToolCall)> = complete_message
            .tool_calls()
            .map(|(ordinal, call)| (ordinal, call.clone()))
            .collect();

        if tool_calls.is_empty() {
            let recorded_at = chrono::Utc::now().timestamp();
            if let Err(err) = coordinator
                .io
                .advance_turn(
                    session_id.clone(),
                    PendingStage::MessageComplete {
                        key: ModelAttemptKey {
                            turn_id,
                            model_step_ordinal: model_step,
                            attempt_ordinal: attempt,
                        },
                        message: complete_message.clone(),
                        usage: attempt_usages.last().copied().flatten(),
                    },
                    recorded_at,
                    Arc::clone(&execution_lock),
                )
                .await
            {
                break 'saga Err(map_io(err));
            }
            let final_message = complete_message
                .clone()
                .bind_zero_tool()
                .expect("zero-Tool completion");
            let recorded_at = chrono::Utc::now().timestamp();
            if let Err(err) = coordinator
                .io
                .advance_turn(
                    session_id.clone(),
                    PendingStage::StepClosed {
                        kind: StepClosedKind::FinishTurn {
                            final_message: complete_message.clone(),
                        },
                        messages: vec![final_message.clone()],
                    },
                    recorded_at,
                    Arc::clone(&execution_lock),
                )
                .await
            {
                break 'saga Err(map_io(err));
            }
            candidate_messages.push(final_message);
            turn_outcome = TurnOutcome::Completed {
                finish_reason: TurnFinishReason::Stop,
            };
            break 'saga Ok(());
        }

        // Tool-bearing: BindingPlanned -> reserve -> Bound -> StepClosing -> dispatch
        let plan = match build_binding_plan(
            &complete_message,
            &tool_calls,
            &tool_catalog,
            session_proto,
            turn_id,
            model_step,
            &coordinator,
        ) {
            Ok(plan) => plan,
            Err(err) => break 'saga Err(err),
        };
        let recorded_at = chrono::Utc::now().timestamp();
        if let Err(err) = coordinator
            .io
            .advance_turn(
                session_id.clone(),
                PendingStage::BindingPlanned {
                    plan: plan.clone(),
                    usage: attempt_usages.last().copied().flatten(),
                },
                recorded_at,
                Arc::clone(&execution_lock),
            )
            .await
        {
            break 'saga Err(map_io(err));
        }
        crash_barrier!('saga, "binding_planned");

        let mut plan = plan;
        match reserve_with_revision(&coordinator, &session_id, &execution_lock, &mut plan).await {
            Ok(()) => {}
            Err(err) => break 'saga Err(err),
        }
        crash_barrier!('saga, "reservation_published");

        let recorded_at = chrono::Utc::now().timestamp();
        if let Err(err) = coordinator
            .io
            .advance_turn(
                session_id.clone(),
                PendingStage::Bound { plan: plan.clone() },
                recorded_at,
                Arc::clone(&execution_lock),
            )
            .await
        {
            break 'saga Err(map_io(err));
        }
        crash_barrier!('saga, "bound");

        let bound_assistant = match plan.bound_message() {
            Ok(message) => message,
            Err(error) => break 'saga Err(ApiError::Internal(error.to_string())),
        };
        let mut closing = StepClosingState {
            plan_key: plan.plan_key,
            messages: vec![bound_assistant.clone()],
            bindings: plan.bindings.clone(),
            closure_cursor: 0,
            closures: Vec::new(),
            current_candidate: None,
        };
        let recorded_at = chrono::Utc::now().timestamp();
        if let Err(err) = coordinator
            .io
            .advance_turn(
                session_id.clone(),
                PendingStage::StepClosing(closing.clone()),
                recorded_at,
                Arc::clone(&execution_lock),
            )
            .await
        {
            break 'saga Err(map_io(err));
        }
        crash_barrier!('saga, "step_closing_empty");

        history.push(bound_assistant.clone());
        candidate_messages.push(bound_assistant.clone());

        let mut stop_batch: Option<lancet_audit::invocation_store::NotExecutedReason> = None;
        for (index, binding) in plan.bindings.iter().enumerate() {
            if stop_batch.is_none() {
                stop_batch = if is_cancelled(request.cancel_flag.as_ref()) {
                    Some(lancet_audit::invocation_store::NotExecutedReason::CancelledBeforeDispatch)
                } else if Instant::now() >= turn_deadline {
                    Some(lancet_audit::invocation_store::NotExecutedReason::BeforeDispatch)
                } else {
                    None
                };
            }
            if let Some(reason) = stop_batch {
                if let Err(err) = publish_not_executed(&coordinator, binding, reason).await {
                    break 'saga Err(err);
                }
                let closure = ClosedBinding {
                    binding: binding.clone(),
                    outcome: ToolResultOutcome::NotExecuted,
                    material: MaterialAvailability::Unavailable {
                        reason: UnavailableMaterialReason::NotExecuted,
                        digest: None,
                        byte_length: None,
                    },
                };
                let tool_result = tool_result_message(&closure);
                closing.messages.push(tool_result.clone());
                closing.closures.push(closure.clone());
                closing.closure_cursor += 1;
                closing.current_candidate = None;
                let recorded_at = chrono::Utc::now().timestamp();
                if let Err(err) = coordinator
                    .io
                    .advance_turn(
                        session_id.clone(),
                        PendingStage::StepClosing(closing.clone()),
                        recorded_at,
                        Arc::clone(&execution_lock),
                    )
                    .await
                {
                    break 'saga Err(map_io(err));
                }
                crash_barrier!('saga, &format!("cursor_{}", closing.closure_cursor));
                history.push(tool_result.clone());
                candidate_messages.push(tool_result);
                continue;
            }

            let call = match plan
                .message
                .parts
                .get(binding.address.part_ordinal as usize)
            {
                Some(MessagePart {
                    semantic: MessageSemantic::ToolCall { call },
                    ..
                }) => call,
                _ => {
                    break 'saga Err(ApiError::Internal(
                        "binding points at a non-ToolCall part".into(),
                    ));
                }
            };

            let public_invocation_id = binding.invocation_id.to_string();
            observer
                .emit(TurnEvent::ToolCallStarted {
                    id: public_invocation_id.clone(),
                    name: binding.method.clone(),
                })
                .await;

            let invocation_binding = InvocationBinding {
                invocation_id: binding.invocation_id,
                identity: BindingIdentity {
                    session_id: session_proto,
                    turn_id,
                    model_step_ordinal: model_step,
                    part_ordinal: binding.address.part_ordinal,
                    method: binding.method.clone(),
                    descriptor_identity: binding.descriptor_identity.clone(),
                    arguments_digest: binding.arguments_digest,
                },
            };
            #[cfg(feature = "test-crash-barriers")]
            if let Err(err) = super::crash_barrier::prepare_dispatch(
                &coordinator,
                &session_id,
                binding.invocation_id,
            )
            .await
            {
                break 'saga Err(map_io(err));
            }
            let params = call.arguments.clone();
            let receipt = cep_client
                .call_raw_with_receipt(invocation_binding, binding.method.clone(), params)
                .await;
            #[cfg(feature = "test-crash-barriers")]
            super::crash_barrier::record_tool_effect(binding.invocation_id);
            let finalize_budget = FinalizeBudget::anchored_at(Instant::now());

            let (exec_outcome, conversation_outcome, canonical_content, evidence) =
                classify_receipt(receipt, &binding.method);

            let draft_result = ResultDraft::new(
                binding.invocation_id,
                binding.binding_digest,
                turn_id,
                binding.address.message,
                binding.address.part_ordinal,
                canonical_content.clone(),
            );
            crash_barrier!('saga, "candidate_computed");

            let (material, draft_for_pin) = match draft_result {
                Ok(draft) => {
                    closing.current_candidate = Some(draft.clone());
                    let recorded_at = chrono::Utc::now().timestamp();
                    if let Err(err) = coordinator
                        .io
                        .advance_turn(
                            session_id.clone(),
                            PendingStage::StepClosing(closing.clone()),
                            recorded_at,
                            Arc::clone(&execution_lock),
                        )
                        .await
                    {
                        break 'saga Err(map_io(err));
                    }
                    crash_barrier!('saga, "candidate_pinned");
                    let publication = coordinator
                        .io
                        .publish_draft(draft.clone(), Arc::clone(&execution_lock))
                        .await;
                    match publication {
                        Ok(DraftPublication::Durable { .. }) => (
                            MaterialAvailability::Available {
                                digest: draft.content_digest,
                                byte_length: draft.byte_length,
                            },
                            Some(draft),
                        ),
                        Ok(DraftPublication::CleanAbsent) => (
                            MaterialAvailability::Unavailable {
                                reason: UnavailableMaterialReason::DraftPublicationFailed,
                                digest: Some(draft.content_digest),
                                byte_length: Some(draft.byte_length),
                            },
                            Some(draft),
                        ),
                        Ok(DraftPublication::Uncertain { candidate }) => {
                            match coordinator
                                .io
                                .revalidate_draft_under_budget(candidate, finalize_budget)
                                .await
                            {
                                Ok(DraftClassification::Durable) => (
                                    MaterialAvailability::Available {
                                        digest: draft.content_digest,
                                        byte_length: draft.byte_length,
                                    },
                                    Some(draft),
                                ),
                                Ok(DraftClassification::CleanAbsent) => (
                                    MaterialAvailability::Unavailable {
                                        reason: UnavailableMaterialReason::DraftPublicationFailed,
                                        digest: Some(draft.content_digest),
                                        byte_length: Some(draft.byte_length),
                                    },
                                    Some(draft),
                                ),
                                Err(err) => break 'saga Err(map_finalize_failure(err)),
                            }
                        }
                        Err(err) => break 'saga Err(map_io(err)),
                    }
                }
                Err(ToolContentError::TooLarge {
                    actual,
                    full_digest,
                    ..
                }) => (
                    MaterialAvailability::Unavailable {
                        reason: UnavailableMaterialReason::OutputTooLarge,
                        digest: Some(full_digest),
                        byte_length: Some(actual as u64),
                    },
                    None,
                ),
            };

            crash_barrier!('saga, "draft_published");

            let terminal_material = match &material {
                MaterialAvailability::Available {
                    digest,
                    byte_length,
                } => lancet_audit::invocation_store::TerminalMaterial::Available {
                    digest: *digest,
                    byte_length: *byte_length,
                },
                MaterialAvailability::Unavailable {
                    reason,
                    digest,
                    byte_length,
                } => lancet_audit::invocation_store::TerminalMaterial::Unavailable {
                    reason: match reason {
                        UnavailableMaterialReason::DraftPublicationFailed => {
                            lancet_audit::invocation_store::UnavailableMaterialReason::DraftPublicationFailed
                        }
                        UnavailableMaterialReason::OutputTooLarge => {
                            lancet_audit::invocation_store::UnavailableMaterialReason::OutputTooLarge
                        }
                        UnavailableMaterialReason::NotExecuted => {
                            lancet_audit::invocation_store::UnavailableMaterialReason::NotExecuted
                        }
                    },
                    digest: *digest,
                    byte_length: *byte_length,
                },
            };

            if let Err(err) = coordinator
                .io
                .publish_terminal_with_budget(
                    binding.invocation_id,
                    exec_outcome.clone(),
                    terminal_material,
                    evidence,
                    finalize_budget,
                )
                .await
            {
                break 'saga Err(map_finalize_failure(err));
            }

            crash_barrier!('saga, "terminal_published");

            let closure = ClosedBinding {
                binding: binding.clone(),
                outcome: conversation_outcome,
                material,
            };
            let content = match &closure.material {
                MaterialAvailability::Available { .. } => Some(canonical_content),
                MaterialAvailability::Unavailable { .. } => None,
            };
            let tool_result = NativeMessage::new(
                MessageRole::Tool,
                None,
                vec![MessagePart::semantic_only(MessageSemantic::ToolResult {
                    invocation_id: binding.invocation_id,
                    name: binding.method.clone(),
                    outcome: conversation_outcome,
                    content,
                })],
            );
            closing.messages.push(tool_result.clone());
            closing.closures.push(closure.clone());
            closing.closure_cursor = (index as u32) + 1;
            closing.current_candidate = None;
            let recorded_at = chrono::Utc::now().timestamp();
            if let Err(err) = coordinator
                .io
                .advance_turn(
                    session_id.clone(),
                    PendingStage::StepClosing(closing.clone()),
                    recorded_at,
                    Arc::clone(&execution_lock),
                )
                .await
            {
                break 'saga Err(map_io(err));
            }
            crash_barrier!('saga, &format!("cursor_{}", closing.closure_cursor));

            history.push(tool_result.clone());
            candidate_messages.push(tool_result);

            observer
                .emit(TurnEvent::ToolCallFinished {
                    id: public_invocation_id,
                    name: binding.method.clone(),
                    ok: matches!(conversation_outcome, ToolResultOutcome::Succeeded),
                    output_len: match &closure.material {
                        MaterialAvailability::Available { byte_length, .. } => {
                            *byte_length as usize
                        }
                        MaterialAvailability::Unavailable {
                            byte_length: Some(n),
                            ..
                        } => *n as usize,
                        _ => 0,
                    },
                    error_summary: None,
                })
                .await;

            let _ = draft_for_pin;
            match conversation_outcome {
                ToolResultOutcome::Indeterminate => {
                    stop_batch = Some(
                        lancet_audit::invocation_store::NotExecutedReason::PrecedingInvocationIndeterminate,
                    );
                }
                ToolResultOutcome::Cancelled => {
                    stop_batch = Some(
                        lancet_audit::invocation_store::NotExecutedReason::CancelledBeforeDispatch,
                    );
                }
                _ => {}
            }
        }

        // Digest the durable bound assistant plus ordered ToolResult closures.
        let digest_source = {
            let mut parts = bound_assistant.parts.clone();
            for closure in &closing.closures {
                parts.push(MessagePart::semantic_only(MessageSemantic::ToolResult {
                    invocation_id: closure.binding.invocation_id,
                    name: closure.binding.method.clone(),
                    outcome: closure.outcome,
                    content: None,
                }));
            }
            NativeMessage::new(MessageRole::Assistant, None, parts)
        };
        let step_closed_kind = match stop_batch {
            None => StepClosedKind::ContinueModel {
                model_step_ordinal: model_step,
                ordered_tool_results: closing.closures.clone(),
                conversation_digest: semantic_digest(&digest_source),
            },
            Some(lancet_audit::invocation_store::NotExecutedReason::CancelledBeforeDispatch) => {
                turn_outcome = TurnOutcome::Cancelled {
                    reason: TurnCancellation::UserCancelled,
                };
                terminal_reason = Some("cancelled by caller".into());
                StepClosedKind::CancelTurn {
                    reason: TurnCancellation::UserCancelled,
                }
            }
            Some(lancet_audit::invocation_store::NotExecutedReason::BeforeDispatch) => {
                let reason = TurnInterruption::ModelAttempt(
                    lattice_conversation::AttemptInterruption::Timeout,
                );
                turn_outcome = TurnOutcome::Interrupted {
                    reason: reason.clone(),
                };
                terminal_reason = Some("timeout".into());
                StepClosedKind::InterruptTurn { reason }
            }
            Some(
                lancet_audit::invocation_store::NotExecutedReason::PrecedingInvocationIndeterminate,
            ) => {
                let Some(invocation) = closing
                    .closures
                    .iter()
                    .find(|closure| matches!(closure.outcome, ToolResultOutcome::Indeterminate))
                    .map(|closure| closure.binding.invocation_id)
                else {
                    break 'saga Err(ApiError::Internal(
                        "indeterminate stop has no indeterminate closure".into(),
                    ));
                };
                let reason = TurnInterruption::InvocationMayHaveOccurred { invocation };
                turn_outcome = TurnOutcome::Interrupted {
                    reason: reason.clone(),
                };
                terminal_reason = Some("tool invocation may have occurred".into());
                StepClosedKind::InterruptTurn { reason }
            }
        };

        let recorded_at = chrono::Utc::now().timestamp();
        if let Err(err) = coordinator
            .io
            .advance_turn(
                session_id.clone(),
                PendingStage::StepClosed {
                    kind: step_closed_kind,
                    messages: closing.messages.clone(),
                },
                recorded_at,
                Arc::clone(&execution_lock),
            )
            .await
        {
            break 'saga Err(map_io(err));
        }

        crash_barrier!('saga, "step_closed");

        if stop_batch.is_some() {
            break 'saga Ok(());
        }

        awaiting_model_continuation = true;
        model_step += 1;
    };

    while let Ok(Some(event)) = tokio::time::timeout(CEP_EVENT_DRAIN_GRACE, agent_rx.recv()).await {
        ordered_events.emit(&mut observer, event).await;
    }
    cep_forwarder.abort();
    while let Ok(event) = agent_rx.try_recv() {
        ordered_events.emit(&mut observer, event).await;
    }
    ordered_events.finish(&mut observer).await;

    if let Some(terminal) = diagnostic_terminal_event(&turn_outcome, terminal_reason.as_deref()) {
        observer.emit(terminal).await;
    }
    observer.emit(TurnEvent::TurnCommitting).await;

    runtime.shutdown().await;

    match saga_result {
        Ok(()) => {
            let closure = LiveTurnClosure {
                session_id: &session_id,
                turn_id,
                prepared: &prepared,
                outcome: turn_outcome,
                messages: candidate_messages,
                usage_aggregate: TurnUsageAggregate::derive(&attempt_usages),
            };
            let commit = close_live_turn(
                &coordinator,
                closure,
                Arc::clone(&execution_lock),
                workspace_history,
            )
            .await;
            match commit {
                Ok(commit) => {
                    observer
                        .emit(TurnEvent::TurnCommitted {
                            session_id: prepared.session_id,
                            commit_id: commit.commit_id,
                            outcome: commit.outcome,
                            origin: commit.origin,
                            workspace_history_checkpoint: commit.workspace_history_checkpoint,
                        })
                        .await;
                }
                Err(err) => {
                    observer
                        .emit(TurnEvent::CommitFailed {
                            reason: err.to_string(),
                        })
                        .await;
                }
            }
        }
        Err(ApiError::EvidenceDurabilityFailed(err)) => {
            observer
                .emit(TurnEvent::EvidenceDurabilityFailed {
                    attempts: err.attempts,
                })
                .await;
        }
        Err(err) => {
            observer
                .emit(TurnEvent::CommitFailed {
                    reason: err.to_string(),
                })
                .await;
        }
    }
}

async fn drain_ready_events(
    agent_rx: &mut mpsc::Receiver<TurnEvent>,
    ordered_events: &mut CepOrderedEmitter,
    observer: &mut ObserverSink,
) {
    while let Ok(event) = agent_rx.try_recv() {
        ordered_events.emit(observer, event).await;
    }
}

fn diagnostic_terminal_event(outcome: &TurnOutcome, reason: Option<&str>) -> Option<TurnEvent> {
    match outcome {
        TurnOutcome::Completed { .. } => None,
        TurnOutcome::Cancelled { .. } => Some(TurnEvent::TurnCancelled {
            reason: Some(
                reason
                    .map(str::to_string)
                    .unwrap_or_else(|| typed_outcome_diagnostic(outcome)),
            ),
        }),
        TurnOutcome::Failed { .. } | TurnOutcome::Interrupted { .. } => {
            Some(TurnEvent::TurnFailed {
                reason: reason
                    .map(str::to_string)
                    .unwrap_or_else(|| typed_outcome_diagnostic(outcome)),
            })
        }
    }
}

fn typed_outcome_diagnostic(outcome: &TurnOutcome) -> String {
    match outcome {
        TurnOutcome::Completed { .. } => "turn completed".to_string(),
        TurnOutcome::Failed { reason } => match reason {
            TurnFailure::ModelRequestFailed => "model request failed".to_string(),
            TurnFailure::ModelStepLimitExceeded => "model step limit exceeded".to_string(),
            TurnFailure::InvalidToolCall { validation } => validation.to_string(),
        },
        TurnOutcome::Cancelled { .. } => "cancelled by caller".to_string(),
        TurnOutcome::Interrupted { reason } => match reason {
            TurnInterruption::ProcessInterruptedBeforeModelAttempt => {
                "process interrupted before model attempt".to_string()
            }
            TurnInterruption::ProcessInterruptedDuringModelAttempt => {
                "process interrupted during model attempt".to_string()
            }
            TurnInterruption::ProcessInterruptedDuringModelRetry => {
                "process interrupted during model retry".to_string()
            }
            TurnInterruption::ModelAttempt(interruption) => match interruption {
                lattice_conversation::AttemptInterruption::StreamError => {
                    "model stream failed".to_string()
                }
                lattice_conversation::AttemptInterruption::Timeout => {
                    "model attempt timed out".to_string()
                }
                lattice_conversation::AttemptInterruption::Cancelled => {
                    "model attempt cancelled".to_string()
                }
                lattice_conversation::AttemptInterruption::OutputLimitExceeded { .. } => {
                    "model output exceeded protocol limit".to_string()
                }
                lattice_conversation::AttemptInterruption::ProcessInterrupted => {
                    "process interrupted during model attempt".to_string()
                }
            },
            TurnInterruption::ProcessInterruptedDuringToolBinding => {
                "process interrupted during Tool binding".to_string()
            }
            TurnInterruption::WhileAwaitingModelContinuation => {
                "process interrupted while awaiting model continuation".to_string()
            }
            TurnInterruption::ProcessInterruptedDuringStepClosing => {
                "process interrupted during step closing".to_string()
            }
            TurnInterruption::InvocationMayHaveOccurred { invocation } => {
                format!("Tool invocation {invocation} may have occurred")
            }
        },
    }
}
enum AttemptHandled {
    Complete(NativeMessage<UnboundToolCall>),
    TurnClosed {
        outcome: TurnOutcome,
        reason: Option<String>,
    },
    Err(ApiError),
}

async fn handle_attempt_outcome(
    step: &ModelStepAttempt,
    attempt_key: ModelAttemptKey,
    session_id: &str,
    coordinator: &TurnCoordinator,
    execution_lock: &Arc<FlockGuard>,
    observer: &mut ObserverSink,
) -> AttemptHandled {
    let recorded_at = chrono::Utc::now().timestamp();
    match &step.status {
        ModelStepStatus::Complete => match step.message.clone() {
            Some(message) => AttemptHandled::Complete(message),
            None => AttemptHandled::Err(ApiError::Internal(
                "complete model attempt carried no message".into(),
            )),
        },
        ModelStepStatus::Rejected(rejection) => {
            if let Err(err) = coordinator
                .io
                .advance_turn(
                    session_id.to_string(),
                    PendingStage::ModelAttemptRejected {
                        key: attempt_key,
                        rejection: rejection.clone(),
                        usage: step.usage,
                    },
                    recorded_at,
                    Arc::clone(execution_lock),
                )
                .await
            {
                return AttemptHandled::Err(map_io(err));
            }
            let validation = match rejection {
                lattice_conversation::AttemptRejection::InvalidToolCall { validation } => {
                    validation.clone()
                }
            };
            let reason = TurnFailure::InvalidToolCall { validation };
            if let Err(err) = coordinator
                .io
                .advance_turn(
                    session_id.to_string(),
                    PendingStage::StepClosed {
                        kind: StepClosedKind::FailTurn {
                            reason: reason.clone(),
                        },
                        messages: Vec::new(),
                    },
                    chrono::Utc::now().timestamp(),
                    Arc::clone(execution_lock),
                )
                .await
            {
                return AttemptHandled::Err(map_io(err));
            }
            AttemptHandled::TurnClosed {
                outcome: TurnOutcome::Failed { reason },
                reason: step.diagnostic.clone(),
            }
        }
        ModelStepStatus::Interrupted(interruption) => {
            if let Err(err) = coordinator
                .io
                .advance_turn(
                    session_id.to_string(),
                    PendingStage::ModelAttemptInterrupted {
                        key: attempt_key,
                        reason: *interruption,
                        usage: step.usage,
                    },
                    recorded_at,
                    Arc::clone(execution_lock),
                )
                .await
            {
                return AttemptHandled::Err(map_io(err));
            }
            let (kind, outcome, reason) = match interruption {
                lattice_conversation::AttemptInterruption::Cancelled => (
                    StepClosedKind::CancelTurn {
                        reason: TurnCancellation::UserCancelled,
                    },
                    TurnOutcome::Cancelled {
                        reason: TurnCancellation::UserCancelled,
                    },
                    Some("cancelled by caller".into()),
                ),
                other => (
                    StepClosedKind::InterruptTurn {
                        reason: TurnInterruption::ModelAttempt(*other),
                    },
                    TurnOutcome::Interrupted {
                        reason: TurnInterruption::ModelAttempt(*other),
                    },
                    step.diagnostic.clone(),
                ),
            };
            let _ = observer;
            if let Err(err) = coordinator
                .io
                .advance_turn(
                    session_id.to_string(),
                    PendingStage::StepClosed {
                        kind,
                        messages: Vec::new(),
                    },
                    chrono::Utc::now().timestamp(),
                    Arc::clone(execution_lock),
                )
                .await
            {
                return AttemptHandled::Err(map_io(err));
            }
            AttemptHandled::TurnClosed { outcome, reason }
        }
    }
}

fn map_step_event(event: ModelStepEvent) -> Option<TurnEvent> {
    match event {
        ModelStepEvent::Opened { .. } => None,
        ModelStepEvent::ReasoningDelta { content } => Some(TurnEvent::ReasoningDelta { content }),
        ModelStepEvent::TokenDelta { content } => Some(TurnEvent::TokenDelta { content }),
    }
}

fn map_io(err: CoordinatorIoError) -> ApiError {
    match err {
        CoordinatorIoError::EvidenceDurabilityFailed { attempts } => {
            ApiError::RecoveryRequired(crate::session::RecoveryRequired::EvidenceDurabilityFailed(
                crate::session::EvidenceDurabilityFailed { attempts },
            ))
        }
        other => ApiError::Internal(other.to_string()),
    }
}

fn map_finalize_failure(
    err: FinalizeFailure<lancet_audit::invocation_store::InvocationStoreError>,
) -> ApiError {
    match err {
        FinalizeFailure::EvidenceDurabilityFailed { attempts } => {
            ApiError::EvidenceDurabilityFailed(crate::session::EvidenceDurabilityFailed {
                attempts,
            })
        }
        FinalizeFailure::Fatal(inner) => {
            ApiError::Internal(format!("terminal publication failed: {inner}"))
        }
    }
}

async fn publish_not_executed(
    coordinator: &TurnCoordinator,
    binding: &PlannedBinding,
    reason: lancet_audit::invocation_store::NotExecutedReason,
) -> Result<(), ApiError> {
    coordinator
        .io
        .publish_terminal_under_budget(
            binding.invocation_id,
            lancet_audit::invocation_store::TerminalOutcome::NotExecuted { reason },
            lancet_audit::invocation_store::TerminalMaterial::Unavailable {
                reason: lancet_audit::invocation_store::UnavailableMaterialReason::NotExecuted,
                digest: None,
                byte_length: None,
            },
            empty_evidence(&binding.method),
        )
        .await
        .map_err(map_finalize_failure)
}

fn is_cancelled(flag: Option<&Arc<AtomicBool>>) -> bool {
    flag.is_some_and(|f| f.load(std::sync::atomic::Ordering::Relaxed))
}

fn user_message(input: &str) -> NativeMessage<BoundToolCall> {
    NativeMessage::new(
        MessageRole::User,
        None,
        vec![MessagePart::semantic_only(MessageSemantic::Text {
            text: input.to_string(),
        })],
    )
}

fn tool_result_message(closure: &ClosedBinding) -> NativeMessage<BoundToolCall> {
    NativeMessage::new(
        MessageRole::Tool,
        None,
        vec![MessagePart::semantic_only(MessageSemantic::ToolResult {
            invocation_id: closure.binding.invocation_id,
            name: closure.binding.method.clone(),
            outcome: closure.outcome,
            content: None,
        })],
    )
}

fn hex_digest(digest: &[u8; 32]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn build_binding_plan(
    message: &NativeMessage<UnboundToolCall>,
    tool_calls: &[(usize, UnboundToolCall)],
    tool_catalog: &[lancet_api::ToolDefinition],
    session_id: SessionId,
    turn_id: TurnId,
    model_step: u32,
    coordinator: &TurnCoordinator,
) -> Result<BindingPlan, ApiError> {
    let mut bindings = Vec::with_capacity(tool_calls.len());
    for (ordinal, call) in tool_calls {
        let descriptor = tool_catalog
            .iter()
            .find(|tool| tool.name == call.name)
            .ok_or_else(|| {
                ApiError::Internal(format!(
                    "tool '{}' missing from descriptor catalog",
                    call.name
                ))
            })?;
        let invocation_id = coordinator.ids.new_invocation_id();
        let address = ToolCallAddress {
            message: ModelMessageAddress {
                session_id,
                turn_id,
                model_step_ordinal: model_step,
            },
            part_ordinal: *ordinal as u32,
        };
        let args_digest = arguments_digest(&call.arguments);
        let binding_digest = PlannedBinding::compute_binding_digest(
            &invocation_id,
            &address,
            &call.name,
            &descriptor.descriptor_identity,
            &args_digest,
        );
        bindings.push(PlannedBinding {
            invocation_id,
            address,
            method: call.name.clone(),
            descriptor_identity: descriptor.descriptor_identity.clone(),
            arguments_digest: args_digest,
            binding_digest,
        });
    }
    let plan = BindingPlan {
        plan_key: PlanKey {
            turn_id,
            model_step_ordinal: model_step,
        },
        message: message.clone(),
        bindings,
    };
    plan.validate()
        .map_err(|err| ApiError::Internal(err.to_string()))?;
    Ok(plan)
}

async fn reserve_with_revision(
    coordinator: &TurnCoordinator,
    session_id: &str,
    execution_lock: &Arc<FlockGuard>,
    plan: &mut BindingPlan,
) -> Result<(), ApiError> {
    loop {
        match coordinator.io.reserve_plan(plan.clone()).await {
            Ok(()) => return Ok(()),
            Err(CoordinatorIoError::InvocationStore(
                lancet_audit::invocation_store::InvocationStoreError::DuplicateInvocation {
                    ..
                },
            )) => {
                let tip = coordinator
                    .io
                    .read_active_turn(session_id.to_string())
                    .await
                    .map_err(map_io)?
                    .ok_or_else(|| {
                        ApiError::Internal("active Turn missing during revision".into())
                    })?;
                let candidates = plan
                    .bindings
                    .iter()
                    .map(|b| b.invocation_id)
                    .collect::<Vec<_>>();
                let facts = coordinator
                    .io
                    .pin_invocation_facts(plan.plan_key, candidates)
                    .await
                    .map_err(map_io)?;
                match lattice_session::pending::reconcile(&tip.snapshot, &facts)
                    .map_err(|e| ApiError::Internal(e.to_string()))?
                {
                    lattice_session::pending::ReconciliationDecision::ReviseCandidates(request) => {
                        let store = coordinator.io.session_store();
                        let ids = Arc::clone(&coordinator.ids);
                        let lock = Arc::clone(execution_lock);
                        let tip_clone = tip.clone();
                        tokio::task::spawn_blocking(move || {
                            store.execute_candidate_revision(
                                &request,
                                &tip_clone,
                                tip_clone.snapshot.recorded_at.ok_or_else(|| {
                                    lattice_session::SessionError::JournalCorruption {
                                        detail:
                                            "pending journal commit has no readable committer time"
                                                .into(),
                                    }
                                })?,
                                &lock,
                                || ids.new_invocation_id(),
                            )
                        })
                        .await
                        .map_err(|e| ApiError::Internal(e.to_string()))?
                        .map_err(|e| ApiError::Internal(e.to_string()))?;
                        let refreshed = coordinator
                            .io
                            .read_active_turn(session_id.to_string())
                            .await
                            .map_err(map_io)?
                            .ok_or_else(|| {
                                ApiError::Internal("active Turn missing after revision".into())
                            })?;
                        match refreshed.snapshot.stage {
                            PendingStage::BindingPlanned {
                                plan: revised_plan, ..
                            } => *plan = revised_plan,
                            _ => {
                                return Err(ApiError::Internal(
                                    "revision did not republish BindingPlanned".into(),
                                ));
                            }
                        }
                    }
                    other => {
                        return Err(ApiError::Internal(format!(
                            "expected candidate revision after collision, got {other:?}"
                        )));
                    }
                }
            }
            Err(err) => return Err(map_io(err)),
        }
    }
}

fn classify_receipt(
    receipt: Result<ExecutionReceipt, CepError>,
    method: &str,
) -> (
    lancet_audit::invocation_store::TerminalOutcome,
    ToolResultOutcome,
    String,
    lancet_audit::invocation_store::TerminalEvidence,
) {
    match receipt {
        Ok(ExecutionReceipt {
            result: Ok(value),
            evidence,
        }) => (
            lancet_audit::invocation_store::TerminalOutcome::Succeeded,
            ToolResultOutcome::Succeeded,
            canonical_json(&value),
            terminal_evidence(evidence),
        ),
        Ok(ExecutionReceipt {
            result: Err(err),
            evidence,
        }) => {
            let (terminal, conversation) = map_cep_outcome(&err);
            (
                terminal,
                conversation,
                bounded_error_content(&err),
                terminal_evidence(evidence),
            )
        }
        Err(err) => {
            let (terminal, conversation) = map_cep_outcome(&err);
            (
                terminal,
                conversation,
                bounded_error_content(&err),
                empty_evidence(method),
            )
        }
    }
}

fn map_cep_outcome(
    err: &CepError,
) -> (
    lancet_audit::invocation_store::TerminalOutcome,
    ToolResultOutcome,
) {
    use lancet_audit::invocation_store::TerminalOutcome;
    use lancet_protocol::error::CepErrorCode;
    if matches!(err.code, CepErrorCode::CallCancelled) {
        return (TerminalOutcome::Cancelled, ToolResultOutcome::Cancelled);
    }
    if matches!(err.code, CepErrorCode::OutcomeIndeterminate) {
        return (
            TerminalOutcome::Indeterminate {
                reason: lancet_audit::invocation_store::IndeterminateReason::MayHaveOccurred,
            },
            ToolResultOutcome::Indeterminate,
        );
    }
    match err.code.family() {
        CepErrorFamily::Validation | CepErrorFamily::Permission | CepErrorFamily::Guard => {
            (TerminalOutcome::Rejected, ToolResultOutcome::Rejected)
        }
        _ => (TerminalOutcome::Failed, ToolResultOutcome::Failed),
    }
}

fn bounded_error_content(err: &CepError) -> String {
    let mut message = err.message.clone();
    if message.len() > BOUNDED_ERROR_CONTENT_BYTES {
        let mut end = BOUNDED_ERROR_CONTENT_BYTES;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
    }
    canonical_json(&serde_json::json!({
        "error": {
            "code": err.code as i32,
            "message": message,
        }
    }))
}

fn terminal_evidence(
    evidence: lancet_protocol::CellEvidenceTriple,
) -> lancet_audit::invocation_store::TerminalEvidence {
    lancet_audit::invocation_store::TerminalEvidence {
        admission: evidence.admission,
        effects: evidence.effects,
        denied: evidence.denied,
    }
}

fn empty_evidence(tool: &str) -> lancet_audit::invocation_store::TerminalEvidence {
    lancet_audit::invocation_store::TerminalEvidence {
        admission: lancet_protocol::AdmissionEvidence {
            version: lancet_protocol::ADMISSION_EVIDENCE_VERSION,
            tool: tool.to_string(),
            approved_capabilities: Vec::new(),
            denied_capabilities: Vec::new(),
            boundary_excluded: Vec::new(),
            max_boundary: lancet_protocol::Trust::In,
        },
        effects: lancet_protocol::ObservedEffectsRecord {
            version: lancet_protocol::OBSERVED_EFFECTS_RECORD_VERSION,
            effects: Vec::new(),
        },
        denied: lancet_protocol::DeniedOperationsRecord {
            version: lancet_protocol::DENIED_OPERATIONS_RECORD_VERSION,
            denied: Vec::new(),
        },
    }
}
struct LiveTurnClosure<'a> {
    session_id: &'a str,
    turn_id: TurnId,
    prepared: &'a PreparedTurn,
    outcome: TurnOutcome,
    messages: Vec<NativeMessage<BoundToolCall>>,
    usage_aggregate: Option<TurnUsageAggregate>,
}

async fn close_live_turn(
    coordinator: &TurnCoordinator,
    closure: LiveTurnClosure<'_>,
    execution_lock: Arc<FlockGuard>,
    workspace_history: Arc<WorkspaceHistory>,
) -> Result<TurnCommit, ApiError> {
    let LiveTurnClosure {
        session_id,
        turn_id,
        prepared,
        outcome,
        messages,
        usage_aggregate,
    } = closure;
    let tip = coordinator
        .io
        .read_active_turn(session_id.to_string())
        .await
        .map_err(map_io)?
        .ok_or_else(|| ApiError::Internal("active Turn missing at close".into()))?;
    let recorded_at = tip.snapshot.recorded_at.ok_or_else(|| {
        ApiError::Internal(
            lattice_session::SessionError::JournalCorruption {
                detail: "pending journal commit has no readable committer time".into(),
            }
            .to_string(),
        )
    })?;
    let created_at = chrono::DateTime::from_timestamp(recorded_at, 0)
        .ok_or_else(|| ApiError::Internal("invalid journal timestamp".into()))?
        .to_rfc3339();
    let profile_hash = tip
        .snapshot
        .root
        .as_ref()
        .or(match &tip.snapshot.stage {
            PendingStage::TurnStarted(facts) => Some(facts),
            _ => None,
        })
        .and_then(|facts| facts.config.digest().ok())
        .map(|d| hex_digest(&d))
        .unwrap_or_else(|| prepared.profile_snapshot_hash.clone());
    let root_config = tip
        .snapshot
        .root
        .clone()
        .or_else(|| match &tip.snapshot.stage {
            PendingStage::TurnStarted(facts) => Some(facts.clone()),
            _ => None,
        });
    let (agent_name, model, provider, profile_json) = match &root_config {
        Some(facts) => (
            facts.config.agent_name.clone(),
            facts.config.model.clone(),
            facts.config.provider.clone(),
            facts.config.profile_snapshot_json.clone(),
        ),
        None => (
            prepared.agent_name.clone(),
            prepared.model.clone(),
            prepared.provider.clone(),
            prepared.profile_snapshot_json.clone(),
        ),
    };
    let commit_message = lattice_session::derive_commit_message(&messages);
    let mut candidate = TurnCommitCandidate {
        session_id: session_id.to_string(),
        turn_id,
        side_parent_ids: Vec::new(),
        messages,
        agent_name,
        profile_snapshot_hash: profile_hash,
        profile_snapshot_json: profile_json,
        model,
        provider,
        outcome,
        origin: ClosureOrigin::Live,
        commit_message,
        usage_aggregate,
        workspace_history_checkpoint: WorkspaceHistoryCheckpoint::Empty,
        created_at,
    };

    let checkpoint = tokio::task::spawn_blocking(move || {
        let _ws_guard = workspace_history
            .lock_workspace_operation(
                lattice_session::LockMode::Shared,
                lattice_session::Blocking::Block,
            )
            .map_err(|err| {
                ApiError::Internal(format!("acquire workspace checkpoint lock: {err}"))
            })?;
        workspace_history
            .current()
            .map_err(|err| ApiError::Internal(format!("read workspace history checkpoint: {err}")))
    })
    .await
    .map_err(|err| ApiError::Internal(format!("read checkpoint task failed: {err}")))??;
    candidate.workspace_history_checkpoint = checkpoint
        .map(WorkspaceHistoryCheckpoint::Oid)
        .unwrap_or(WorkspaceHistoryCheckpoint::Empty);

    let session_store = coordinator.io.session_store();
    let lock = Arc::clone(&execution_lock);
    let session_id_owned = session_id.to_string();
    tokio::task::spawn_blocking(move || {
        session_store.advance_pending_turn(
            &session_id_owned,
            PendingStage::TurnClosing(TurnClosingFacts { turn_id, candidate }),
            recorded_at,
            lock.as_ref(),
        )
    })
    .await
    .map_err(|err| ApiError::Internal(format!("publish TurnClosing task failed: {err}")))?
    .map_err(|err| ApiError::Internal(format!("publish TurnClosing: {err}")))?;

    #[cfg(feature = "test-crash-barriers")]
    super::crash_barrier::hit(coordinator, session_id, "turn_closing")
        .await
        .map_err(map_io)?;

    let session_store = coordinator.io.session_store();
    let lock = Arc::clone(&execution_lock);
    let session_id_owned = session_id.to_string();
    tokio::task::spawn_blocking(move || {
        session_store.finalize_turn(&session_id_owned, &turn_id, lock.as_ref())
    })
    .await
    .map_err(|err| ApiError::Internal(format!("finalize Turn task failed: {err}")))?
    .map_err(|err| ApiError::Internal(format!("finalize Turn: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_audit::invocation_store::{
        BindingIdentity as StoreBindingIdentity, InvocationState, InvocationStore,
        NotExecutedReason, ReservationBinding, TerminalOutcome,
    };
    use lancet_protocol::InvocationId;
    use lattice_session::{ProjectStoreKey, StateLayout};

    fn empty_triple(tool: &str) -> lancet_protocol::CellEvidenceTriple {
        lancet_protocol::CellEvidenceTriple {
            admission: empty_evidence(tool).admission,
            effects: empty_evidence(tool).effects,
            denied: empty_evidence(tool).denied,
        }
    }

    #[test]
    fn indeterminate_cep_error_maps_to_may_have_occurred() {
        let mut err = CepError::internal_error("history durability failed");
        err.code = lancet_protocol::CepErrorCode::OutcomeIndeterminate;
        let (terminal, conversation) = map_cep_outcome(&err);
        assert!(matches!(
            terminal,
            TerminalOutcome::Indeterminate {
                reason: lancet_audit::invocation_store::IndeterminateReason::MayHaveOccurred
            }
        ));
        assert_eq!(conversation, ToolResultOutcome::Indeterminate);
    }

    #[test]
    fn typed_outcome_supplies_non_empty_diagnostic_fallbacks() {
        let invalid = TurnOutcome::Failed {
            reason: TurnFailure::InvalidToolCall {
                validation: lattice_conversation::ToolCallValidation::MalformedArguments {
                    call_ordinal: 2,
                },
            },
        };
        let timeout = TurnOutcome::Interrupted {
            reason: TurnInterruption::ModelAttempt(
                lattice_conversation::AttemptInterruption::Timeout,
            ),
        };
        let cancelled = TurnOutcome::Cancelled {
            reason: TurnCancellation::UserCancelled,
        };

        for outcome in [&invalid, &timeout] {
            let Some(TurnEvent::TurnFailed { reason }) = diagnostic_terminal_event(outcome, None)
            else {
                panic!("non-completed outcome must emit TurnFailed");
            };
            assert!(!reason.is_empty());
        }
        let Some(TurnEvent::TurnCancelled { reason }) = diagnostic_terminal_event(&cancelled, None)
        else {
            panic!("cancelled outcome must emit TurnCancelled");
        };
        assert_eq!(reason.as_deref(), Some("cancelled by caller"));
    }
    #[test]
    fn successful_oversize_receipt_keeps_success_and_records_unavailable_material() {
        let value = serde_json::Value::String("x".repeat(65_535));
        let receipt = Ok(ExecutionReceipt {
            result: Ok(value),
            evidence: empty_triple("oversize.test"),
        });
        let (terminal, conversation, content, _) = classify_receipt(receipt, "oversize.test");
        assert_eq!(terminal, TerminalOutcome::Succeeded);
        assert_eq!(conversation, ToolResultOutcome::Succeeded);
        assert_eq!(content.len(), 65_537);
        let invocation = InvocationId::parse("00000000-0000-4000-8000-000000000001").unwrap();
        let turn_id = TurnId::parse("00000000-0000-4000-8000-000000000002").unwrap();
        let error = ResultDraft::new(
            invocation,
            [7; 32],
            turn_id,
            ModelMessageAddress {
                session_id: SessionId::parse("00000000000040008000000000000003").unwrap(),
                turn_id,
                model_step_ordinal: 0,
            },
            0,
            content,
        )
        .expect_err("plus-one canonical content is not persisted as a draft");
        assert!(matches!(
            error,
            ToolContentError::TooLarge {
                limit: 65_536,
                actual: 65_537,
                ..
            }
        ));
    }

    #[test]
    fn bounded_error_content_truncates_utf8_at_a_char_boundary() {
        let mut error = CepError::internal_error("界".repeat(2_000));
        error.message.push('x');
        let content = bounded_error_content(&error);
        let value: serde_json::Value = serde_json::from_str(&content).unwrap();
        let message = value["error"]["message"].as_str().unwrap();
        assert!(message.len() <= BOUNDED_ERROR_CONTENT_BYTES);
        assert!(message.is_char_boundary(message.len()));
    }

    #[test]
    fn finalize_budget_expiry_is_a_typed_api_error() {
        let error = map_finalize_failure(FinalizeFailure::EvidenceDurabilityFailed { attempts: 3 });
        assert!(matches!(
            error,
            ApiError::EvidenceDurabilityFailed(crate::session::EvidenceDurabilityFailed {
                attempts: 3
            })
        ));
    }

    #[tokio::test]
    async fn skipped_binding_is_terminalized_before_journal_closure() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = StateLayout::in_state_root(tmp.path());
        let key = ProjectStoreKey::derive(tmp.path()).unwrap();
        let session =
            Arc::new(lattice_session::GitSessionGraphStore::create_layout(&layout).unwrap());
        let invocations = Arc::new(InvocationStore::create(&layout, &key).unwrap());
        let turn_id = TurnId::parse("00000000-0000-4000-8000-000000000002").unwrap();
        let session_id = SessionId::parse("00000000000040008000000000000003").unwrap();
        let invocation_id = InvocationId::parse("00000000-0000-4000-8000-000000000001").unwrap();
        let address = ToolCallAddress {
            message: ModelMessageAddress {
                session_id,
                turn_id,
                model_step_ordinal: 0,
            },
            part_ordinal: 0,
        };
        let arguments_digest = arguments_digest(&serde_json::json!({}));
        let binding_digest = lattice_session::pending::binding_digest(
            &invocation_id,
            &address,
            "observe.workspace",
            &"d".repeat(64),
            &arguments_digest,
        );
        let binding = PlannedBinding {
            invocation_id,
            address,
            method: "observe.workspace".into(),
            descriptor_identity: "d".repeat(64),
            arguments_digest,
            binding_digest,
        };
        invocations
            .reserve_batch(
                PlanKey {
                    turn_id,
                    model_step_ordinal: 0,
                },
                &[ReservationBinding {
                    invocation_id,
                    identity: StoreBindingIdentity {
                        session_id,
                        turn_id,
                        model_step_ordinal: 0,
                        part_ordinal: 0,
                        method: binding.method.clone(),
                        descriptor_identity: binding.descriptor_identity.clone(),
                        arguments_digest,
                    },
                }],
            )
            .unwrap();
        let coordinator = TurnCoordinator::new(
            super::super::coordinator::CoordinatorStoreIo::new(session, Arc::clone(&invocations)),
            Arc::new(super::super::coordinator::RandomIdSource),
        );
        publish_not_executed(&coordinator, &binding, NotExecutedReason::BeforeDispatch)
            .await
            .unwrap();
        let (_, snapshot) = invocations.snapshot().unwrap();
        assert!(matches!(
            snapshot.invocations[&invocation_id].state,
            InvocationState::Terminal {
                outcome: TerminalOutcome::NotExecuted {
                    reason: NotExecutedReason::BeforeDispatch
                },
                ..
            }
        ));
    }
}
