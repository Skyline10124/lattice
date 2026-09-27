// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use lancet_protocol::{
    CellEvidenceTriple, CepError, CepErrorCode, CepErrorFamily, InvocationBinding,
    ModelMessageAddress,
};
use lattice_conversation::TokenUsage;

/// 上游先脱敏；本函数仅按 Unicode 字符数执行持久诊断上限。
pub fn bound_provider_diagnostic(already_redacted: &str) -> String {
    already_redacted
        .chars()
        .take(MAX_PROVIDER_ERROR_CHARS)
        .collect()
}

pub fn prepare_terminal(
    binding: &InvocationBinding,
    result: std::result::Result<serde_json::Value, CepError>,
    evidence: CellEvidenceTriple,
) -> Result<InvocationTerminal> {
    validate::evidence(binding, &evidence)?;
    let payload = match result {
        Ok(value) => {
            let bytes = lancet_protocol::canonical_json(&value);
            let digest = lattice_conversation::tool_content_digest(&bytes);
            let byte_length = bytes.len() as u64;
            ResultPayload::Succeeded(if bytes.len() > MAX_TOOL_CONTENT_BYTES {
                SuccessContent::OutputTooLarge {
                    digest,
                    byte_length,
                }
            } else {
                SuccessContent::Json {
                    value,
                    digest,
                    byte_length,
                }
            })
        }
        Err(e) => match e.code {
            CepErrorCode::CallCancelled => {
                ResultPayload::Cancelled(CancelledReason::DuringDispatch)
            }
            CepErrorCode::OutcomeIndeterminate => {
                ResultPayload::Indeterminate(IndeterminateReason::MayHaveOccurred)
            }
            code if matches!(
                code.family(),
                CepErrorFamily::Validation | CepErrorFamily::Permission | CepErrorFamily::Guard
            ) =>
            {
                ResultPayload::Rejected(Box::new(e))
            }
            _ => ResultPayload::Failed(Box::new(e)),
        },
    };
    Ok(InvocationTerminal {
        result: StoredToolResult {
            invocation_id: binding.invocation_id,
            name: binding.identity.method.clone(),
            payload,
        },
        evidence: InvocationEvidence::Recorded(Box::new(evidence)),
    })
}

pub(super) fn stopping_cause(
    binding: &InvocationBinding,
    payload: &ResultPayload,
) -> Option<StopCause> {
    match payload {
        ResultPayload::Succeeded(_) | ResultPayload::Failed(_) | ResultPayload::Rejected(_) => None,
        ResultPayload::Cancelled(_)
        | ResultPayload::NotExecuted(NotExecutedReason::CancelledBeforeDispatch) => {
            Some(StopCause::Cancelled)
        }
        ResultPayload::Indeterminate(_) => Some(StopCause::InvocationIndeterminate {
            invocation_id: binding.invocation_id,
        }),
        ResultPayload::NotExecuted(NotExecutedReason::DeadlineBeforeDispatch) => {
            Some(StopCause::Deadline)
        }
        ResultPayload::NotExecuted(NotExecutedReason::BeforeDispatch) => {
            Some(StopCause::ProcessExit)
        }
        ResultPayload::NotExecuted(NotExecutedReason::PrecedingInvocationIndeterminate) => None,
    }
}
/// 返回第一个停止事实；继续类终态不参与严重性比较。
pub fn first_stopping_cause(slots: &[ClosedInvocation]) -> Result<Option<StopCause>> {
    let active: Vec<_> = slots
        .iter()
        .map(|s| InvocationSlot {
            binding: s.binding.clone(),
            state: SlotState::Terminal(s.terminal.clone()),
        })
        .collect();
    validate::slot_order(&active)?;
    Ok(slots
        .iter()
        .find_map(|s| stopping_cause(&s.binding, &s.terminal.result.payload)))
}
pub(super) fn fill_slots(
    slots: &[InvocationSlot],
    stop: DispatchStop,
) -> Result<(Vec<ClosedInvocation>, Vec<ClosedInvocation>)> {
    validate::slot_order(slots)?;
    let mut closed = Vec::with_capacity(slots.len());
    let mut changed = Vec::new();
    let mut indeterminate = false;
    for slot in slots {
        let terminal = match &slot.state {
            SlotState::Terminal(t) => t.clone(),
            SlotState::Started => InvocationTerminal {
                result: StoredToolResult {
                    invocation_id: slot.binding.invocation_id,
                    name: slot.binding.identity.method.clone(),
                    payload: ResultPayload::Indeterminate(IndeterminateReason::MayHaveOccurred),
                },
                evidence: InvocationEvidence::Unavailable {
                    reason: EvidenceUnavailableReason::ProcessExit,
                },
            },
            SlotState::Registered => {
                let reason = if indeterminate {
                    NotExecutedReason::PrecedingInvocationIndeterminate
                } else {
                    match stop {
                        DispatchStop::Cancelled => NotExecutedReason::CancelledBeforeDispatch,
                        DispatchStop::Deadline => NotExecutedReason::DeadlineBeforeDispatch,
                        DispatchStop::ProcessExit => NotExecutedReason::BeforeDispatch,
                    }
                };
                InvocationTerminal {
                    result: StoredToolResult {
                        invocation_id: slot.binding.invocation_id,
                        name: slot.binding.identity.method.clone(),
                        payload: ResultPayload::NotExecuted(reason),
                    },
                    evidence: InvocationEvidence::Unavailable {
                        reason: EvidenceUnavailableReason::NotDispatched,
                    },
                }
            }
        };
        indeterminate |= matches!(terminal.result.payload, ResultPayload::Indeterminate(_));
        let entry = ClosedInvocation {
            binding: slot.binding.clone(),
            terminal,
        };
        if !matches!(slot.state, SlotState::Terminal(_)) {
            changed.push(entry.clone());
        }
        closed.push(entry);
    }
    first_stopping_cause(&closed)?;
    Ok((closed, changed))
}

pub(super) fn usage<'a>(steps: impl IntoIterator<Item = &'a StepRecord>) -> UsageSummary {
    let mut total = TokenUsage {
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
    };
    let mut unknown = 0;
    for attempt in steps.into_iter().flat_map(|s| &s.attempts) {
        if let Some(u) = attempt.usage {
            total = total.saturating_add(u);
        } else {
            unknown += 1;
        }
    }
    if unknown == 0 {
        UsageSummary::Complete { total }
    } else {
        UsageSummary::Partial {
            known: total,
            unknown_attempts: unknown,
        }
    }
}
pub(super) fn summary(state: &TurnState) -> Option<TurnSummary> {
    match state {
        TurnState::Running => None,
        TurnState::Closed {
            outcome, notice, ..
        } => Some(TurnSummary {
            outcome: match outcome {
                TurnOutcome::Completed => PublicTurnOutcome::Completed,
                TurnOutcome::Cancelled => PublicTurnOutcome::Cancelled,
                TurnOutcome::Failed { .. } => PublicTurnOutcome::Failed,
            },
            notice: notice.clone(),
        }),
    }
}
pub(super) fn outcome(step: &StepRecord) -> Result<(TurnOutcome, Option<Notice>)> {
    let StepState::Closed { end, .. } = &step.state else {
        return Err(corrupt("steps", "end", "active final step"));
    };
    let cause = match end {
        StepEnd::Finish { .. } => return Ok((TurnOutcome::Completed, None)),
        StepEnd::Stop { cause, .. } => cause,
        StepEnd::Continue { .. } => {
            return Err(corrupt("steps", "end", "continue is not a turn closure"));
        }
    };
    let text = match cause {
        StopCause::ProviderError { .. } => match step.attempts.last().map(|a| &a.end) {
            Some(AttemptEnd::Stopped {
                cause: actual,
                diagnostic,
            }) if actual == cause => diagnostic.provider_error.clone().ok_or_else(|| {
                corrupt("steps", "diagnostic", "missing final provider diagnostic")
            })?,
            _ => {
                return Err(corrupt(
                    "steps",
                    "diagnostic",
                    "provider cause does not match final attempt",
                ));
            }
        },
        StopCause::Cancelled => "cancelled by caller".into(),
        StopCause::Deadline => "turn deadline exceeded".into(),
        StopCause::OutputLimit => "model output limit exceeded".into(),
        StopCause::InvalidBatch => "invalid tool call batch".into(),
        StopCause::ProcessExit => "process exited before turn closure".into(),
        StopCause::InvocationIndeterminate { .. } => {
            "tool execution outcome is indeterminate".into()
        }
        StopCause::ModelStepLimit => "model step limit exceeded".into(),
    };
    if text.chars().count() > MAX_PROVIDER_ERROR_CHARS {
        return Err(corrupt(
            "steps",
            "diagnostic",
            "provider diagnostic exceeds bound",
        ));
    }
    let (outcome, level) = if *cause == StopCause::Cancelled {
        (TurnOutcome::Cancelled, NoticeLevel::Info)
    } else {
        (
            TurnOutcome::Failed {
                reason: cause.clone(),
            },
            NoticeLevel::Error,
        )
    };
    Ok((outcome, Some(Notice { level, text })))
}
pub(super) fn closing_step(state: &DurableTurnState) -> StepRecord {
    if let Some(last) = state.steps.last()
        && matches!(last.state, StepState::Active { .. })
    {
        return last.clone();
    }
    StepRecord {
        key: StepKey {
            turn_id: state.turn.turn_id,
            ordinal: state.steps.len() as u32,
        },
        attempts: Vec::new(),
        state: StepState::Active {
            active: ActiveStep::RetryReady { next_attempt: 0 },
        },
    }
}
pub(super) fn closure(
    state: &DurableTurnState,
    mut record: StepRecord,
    node: Option<(NodeId, Node)>,
    invocations: Vec<ClosedInvocation>,
    end: StepEnd,
    changed: Vec<ClosedInvocation>,
    now: Timestamp,
) -> Result<ClosureWrites> {
    record.state = StepState::Closed { end, invocations };
    let (outcome, notice) = outcome(&record)?;
    let earlier = state
        .steps
        .iter()
        .filter(|s| s.key.ordinal < record.key.ordinal);
    let usage = usage(earlier.chain(std::iter::once(&record)));
    let current = node.as_ref().map_or(state.session.current, |(id, _)| *id);
    Ok(ClosureWrites {
        closed_step: record,
        node,
        turn_state: TurnState::Closed {
            outcome,
            notice,
            usage,
            ended_at: now,
        },
        session_current: current,
        session_updated_at: now,
        changed_invocations: changed,
    })
}
pub(super) fn node_for(
    state: &DurableTurnState,
    record: &StepRecord,
    reply: lattice_conversation::NativeMessage<lattice_conversation::BoundToolCall>,
    slots: &[ClosedInvocation],
) -> (NodeId, Node) {
    (
        state.next_node,
        Node::Step {
            parent: state.session.current,
            turn_id: state.turn.turn_id,
            step_ordinal: record.key.ordinal,
            content: StepContent {
                assistant: reply,
                results: slots.iter().map(|s| s.terminal.result.clone()).collect(),
            },
        },
    )
}

/// 只消费持久事实，不读取时钟、配置文件、工作区或执行组件。
pub fn plan_auto_close(state: &DurableTurnState, now: Timestamp) -> Result<AutoClosePlan> {
    validate::durable(state)?;
    if let Some(summary) = summary(&state.turn.state) {
        return Ok(AutoClosePlan::AlreadyClosed { summary });
    }
    let mut record = closing_step(state);
    let mut node = None;
    let mut invocations = Vec::new();
    let mut changed = Vec::new();
    let mut cause = StopCause::ProcessExit;
    if let StepState::Active { active } = &record.state {
        match active {
            ActiveStep::Requesting { attempt } => record.attempts.push(AttemptRecord {
                attempt: *attempt,
                origin: validate::provider(&state.turn.config.provider)?,
                usage: None,
                end: AttemptEnd::Stopped {
                    cause: StopCause::ProcessExit,
                    diagnostic: AttemptDiagnostic::default(),
                },
            }),
            ActiveStep::RetryReady { .. } => {}
            ActiveStep::Executing { reply, slots } => {
                (invocations, changed) = fill_slots(slots, DispatchStop::ProcessExit)?;
                cause = first_stopping_cause(&invocations)?.unwrap_or(StopCause::ProcessExit);
                node = Some(node_for(state, &record, reply.clone(), &invocations));
            }
        }
    }
    let end = StepEnd::Stop {
        node: node.as_ref().map(|(id, _)| *id),
        cause,
    };
    Ok(AutoClosePlan::Close(Box::new(closure(
        state,
        record,
        node,
        invocations,
        end,
        changed,
        now,
    )?)))
}

pub(super) fn completed(reply: &CompleteReply, session: SessionId) -> AttemptRecord {
    AttemptRecord {
        attempt: reply.key.attempt_ordinal,
        origin: reply.origin,
        usage: reply.usage,
        end: AttemptEnd::Completed {
            step: ModelMessageAddress {
                session_id: session,
                turn_id: reply.key.turn_id,
                model_step_ordinal: reply.key.model_step_ordinal,
            },
        },
    }
}
