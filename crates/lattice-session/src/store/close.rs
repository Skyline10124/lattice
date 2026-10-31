// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use lancet_protocol::{CellEvidenceTriple, CepError, CepErrorCode, InvocationBinding};

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
                ResultPayload::Indeterminate(IndeterminateCause::Cancelled)
            }
            CepErrorCode::OutcomeIndeterminate => {
                ResultPayload::Indeterminate(IndeterminateCause::Reported)
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

/// 只有 LANCET 给出确定结果时才继续下一调用；其余结局停止后续分发。
pub fn stops_batch(payload: &ResultPayload) -> bool {
    !matches!(
        payload,
        ResultPayload::Succeeded(_) | ResultPayload::Failed(_)
    )
}

/// 槽位自身决定的停止原因。NotExecuted 与 NoResult 不带原因，由 Step 的外部停止说明。
fn own_stop(binding: &InvocationBinding, payload: &ResultPayload) -> Option<StopCause> {
    match payload {
        ResultPayload::Indeterminate(IndeterminateCause::Cancelled) => Some(StopCause::Cancelled),
        ResultPayload::Indeterminate(IndeterminateCause::Reported) => {
            Some(StopCause::InvocationIndeterminate {
                invocation_id: binding.invocation_id,
            })
        }
        _ => None,
    }
}

/// 按对照表从槽位 `progress` 给出结果与证据。
pub fn resolve_slot(slot: &InvocationSlot) -> (StoredToolResult, InvocationEvidence) {
    match &slot.progress {
        SlotProgress::Registered => (
            StoredToolResult {
                invocation_id: slot.binding.invocation_id,
                name: slot.binding.identity.method.clone(),
                payload: ResultPayload::NotExecuted,
            },
            InvocationEvidence::Unavailable {
                reason: EvidenceUnavailableReason::NotDispatched,
            },
        ),
        SlotProgress::Started => (
            StoredToolResult {
                invocation_id: slot.binding.invocation_id,
                name: slot.binding.identity.method.clone(),
                payload: ResultPayload::Indeterminate(IndeterminateCause::NoResult),
            },
            InvocationEvidence::Unavailable {
                reason: EvidenceUnavailableReason::NoResult,
            },
        ),
        SlotProgress::Terminal { result, evidence } => (result.clone(), evidence.clone()),
    }
}

/// 按对照表从尝试 `progress` 给出终态与诊断。
pub fn resolve_attempt(attempt: &AttemptRecord) -> (AttemptEnd, AttemptDiagnostic) {
    match &attempt.progress {
        AttemptProgress::Sent => (
            AttemptEnd::Stopped {
                cause: StopCause::ProcessExit,
            },
            AttemptDiagnostic::default(),
        ),
        AttemptProgress::Ended { end, diagnostic } => (end.clone(), diagnostic.clone()),
    }
}

/// 按片段顺序返回第一个由槽位自身决定的停止原因。
pub fn first_stopping_cause(slots: &[InvocationSlot]) -> Result<Option<StopCause>> {
    Ok(slots
        .iter()
        .find_map(|s| own_stop(&s.binding, &resolve_slot(s).0.payload)))
}

/// Step 的停止原因：槽位自身的停止优先；否则取关闭 Step 的外部停止。
/// 所有槽位都已有确定结果时，只有进程退出能让 Step 停下；取消或 Deadline 应先以 Continue 关闭，
/// 再在同一事务预留并停止下一步，此时返回 `None`。
pub fn step_stop_cause(slots: &[InvocationSlot], stop: DispatchStop) -> Result<Option<StopCause>> {
    if let Some(cause) = first_stopping_cause(slots)? {
        return Ok(Some(cause));
    }
    let unfinished = slots
        .iter()
        .any(|s| stops_batch(&resolve_slot(s).0.payload));
    Ok((unfinished || stop == DispatchStop::ProcessExit).then(|| stop.into()))
}

/// 按对照表从 Step 的 `progress` 与槽位给出结束方式。
pub fn resolve_step(progress: &StepProgress, slots: &[InvocationSlot]) -> Result<StepEnd> {
    match progress {
        StepProgress::Reserved | StepProgress::Requesting => Ok(StepEnd::Stop {
            cause: StopCause::ProcessExit,
        }),
        StepProgress::Executing => Ok(StepEnd::Stop {
            cause: step_stop_cause(slots, DispatchStop::ProcessExit)?
                .unwrap_or(StopCause::ProcessExit),
        }),
        StepProgress::Closed { end } => Ok(end.clone()),
    }
}

pub(super) fn usage<'a>(attempts: impl IntoIterator<Item = &'a AttemptRecord>) -> UsageSummary {
    UsageSummary::from_attempts(
        attempts
            .into_iter()
            .map(|attempt| (attempt.usage, attempt.usage_kind, attempt.cost)),
    )
}

pub(super) fn outcome(
    progress: &StepProgress,
    attempts: &[AttemptRecord],
    slots: &[InvocationSlot],
) -> Result<(TurnOutcome, Option<Notice>)> {
    let end = resolve_step(progress, slots)?;
    let cause = match &end {
        StepEnd::Finish => return Ok((TurnOutcome::Completed, None)),
        StepEnd::Stop { cause } => cause,
        StepEnd::Continue => {
            return Err(corrupt("nodes", "end", "continue is not a turn closure"));
        }
    };
    let text = match cause {
        StopCause::ProviderError { .. } => attempts
            .last()
            .map(resolve_attempt)
            .and_then(|(_, diagnostic)| diagnostic.provider_error)
            .ok_or_else(|| corrupt("nodes", "diagnostic", "missing final provider diagnostic"))?,
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
pub(super) fn summary(
    progress: &StepProgress,
    attempts: &[AttemptRecord],
    slots: &[InvocationSlot],
) -> Result<TurnSummary> {
    let (outcome, notice) = outcome(progress, attempts, slots)?;
    Ok(TurnSummary {
        outcome: match outcome {
            TurnOutcome::Completed => PublicTurnOutcome::Completed,
            TurnOutcome::Cancelled => PublicTurnOutcome::Cancelled,
            TurnOutcome::Failed { .. } => PublicTurnOutcome::Failed,
        },
        notice,
        progress: progress.clone(),
    })
}

/// 为每个带等待时长的尝试生成一条 Warn 级重试提示。
pub fn retry_notices(attempts: &[AttemptRecord]) -> Result<Vec<RetryNotice>> {
    let mut notices = Vec::new();
    for (index, attempt) in attempts.iter().enumerate() {
        let Some(delay_ms) = attempt.retry_delay_ms else {
            continue;
        };
        let (_, diagnostic) = resolve_attempt(attempt);
        let text = diagnostic
            .provider_error
            .ok_or_else(|| corrupt("nodes", "diagnostic", "missing retry provider diagnostic"))?;
        notices.push(RetryNotice {
            attempt: index as u32,
            delay_ms,
            notice: Notice {
                level: NoticeLevel::Warn,
                text,
            },
        });
    }
    Ok(notices)
}

pub(super) fn completed(reply: &CompleteReply) -> AttemptRecord {
    AttemptRecord {
        origin: reply.origin.clone(),
        usage: reply
            .usage
            .and_then(lattice_conversation::TokenUsage::normalized),
        usage_kind: reply.usage_kind,
        cost: reply.cost,
        progress: AttemptProgress::Ended {
            end: AttemptEnd::Completed,
            diagnostic: AttemptDiagnostic::default(),
        },
        retry_delay_ms: None,
    }
}
