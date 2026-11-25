// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `lattice-session` 与 `lattice-context` 线上类型片段及按架构文档规则手写的内部一致场景构造器。
//!
//! 同一份场景输入同时生成扁平对话条目（`ConversationEntry`）与查询详情
//! （`TurnDetail`、`StepDetail`、`AttemptDetail`、`SlotDetail`、`SessionLog`、`SessionUsage`），
//! 不依赖任何物理持久类型（`Node`、`TurnStartMeta`、`AttemptRecord`、`InvocationSlot` 等），
//! 也不调用生产派生函数或摘要函数。

use std::collections::BTreeMap;

use lattice_context::{
    Channel, ContextEdit, ContextInput, ContextInspection, ContextOp, EffectiveEntrySummary,
    NodeStatus, OperatedNode, PathNodeKind,
};
use lattice_conversation::{
    BoundToolCall, CostEstimate, CostSummary, IndeterminateCause, MessagePart, MessageRole,
    MessageSemantic, ModelMessageAddress, NativeMessage, ReplayOrigin, ResultPayload, SessionId,
    StepContent, StoredToolResult, SuccessContent, TokenSummary, TokenUsage, ToolResultOutcome,
    TurnId, UsageCompleteness, UsageSummary,
};
use lattice_session::store::{
    AttemptDetail, AttemptEnd, AttemptEndDetail, AttemptProgressDetail, BindingIdentity,
    ConversationEntry, LogRow, MaterialSummary, NodeHeader, NodeId, NodeKind, Notice, NoticeLevel,
    PublicTurnOutcome, RetryNotice, SessionListRow as StoreSessionListRow, SessionLog,
    SessionRecord, SessionUsage, SlotDetail, SlotReason, SlotState, StepDetail, StepEnd, StepKey,
    StepProgress, StopCause, Timestamp, TurnDetail, TurnLogSummary, TurnSummary, TurnUsageRow,
};

use super::{
    ARGS_CARGO_TOML_DIGEST_HEX, ARGS_MISSING_TXT_DIGEST_HEX, ARGS_SLEEP_CMD_DIGEST_HEX,
    CONTENT_LINES_42_BYTES, CONTENT_LINES_42_DIGEST_HEX, CONTENT_OVERSIZE_BYTES,
    CONTENT_OVERSIZE_DIGEST_HEX, CaseBook, agent_coder,
    conversation::{
        bound_call_read_cargo, bound_call_read_missing, bound_call_sleep_cmd, cost_estimate_exact,
        image_ref_sample, opaque_state_sample, replay_origin_openai, success_content_json_sample,
        token_usage_full, tool_error_not_found,
    },
    hex_32, invocation_id_1, invocation_id_2, model_gpt5, project_path_non_utf8, provider_openai,
    session_id_primary, ts_accepted, ts_created, ts_ended, ts_step_0, turn_id_1, turn_id_2,
    turn_id_3,
};

/// 单个场景从同一份输入同时产出的扁平条目与投影视图。
pub(super) struct BuiltScenario {
    pub(super) entries: Vec<ConversationEntry>,
    pub(super) turn_detail: TurnDetail,
    pub(super) session_log: SessionLog,
    pub(super) session_usage: SessionUsage,
}

struct TurnSpec {
    session_id: SessionId,
    turn_id: TurnId,
    user_parts: Vec<MessageSemantic<BoundToolCall>>,
    steps: Vec<StepSpec>,
}

pub(super) fn channel_cli() -> Channel {
    Channel::parse("cli").expect("固定通道名 cli 应合法")
}

pub(super) fn channel_tui() -> Channel {
    Channel::parse("tui").expect("固定通道名 tui 应合法")
}

fn user_text_parts(text: &str) -> Vec<MessageSemantic<BoundToolCall>> {
    vec![MessageSemantic::Text {
        text: text.to_owned(),
    }]
}

fn user_interleaved_image_parts(before: &str, after: &str) -> Vec<MessageSemantic<BoundToolCall>> {
    vec![
        MessageSemantic::Text {
            text: before.to_owned(),
        },
        MessageSemantic::Image {
            image: image_ref_sample(),
        },
        MessageSemantic::Text {
            text: after.to_owned(),
        },
    ]
}

pub(super) fn sample_context_op_entry() -> ConversationEntry {
    let op = ContextOp::new(
        channel_tui(),
        vec![
            ContextEdit::omit_node(NodeId(1)),
            ContextEdit::strip_reasoning(NodeId(2)),
        ],
    )
    .expect("固定 ContextOp 应合法");
    ConversationEntry::ContextOp {
        node: NodeId(4),
        accepted_at: ts_ended(),
        op,
    }
}

pub(super) fn context_inspection_full() -> ContextInspection {
    let mut channel_counts = BTreeMap::new();
    channel_counts.insert(channel_cli(), 1);
    channel_counts.insert(channel_tui(), 1);
    ContextInspection {
        entries: vec![
            EffectiveEntrySummary::Input {
                node: NodeId(1),
                channel: channel_cli(),
                text_bytes: 54,
                image_count: 1,
                image_bytes: image_ref_sample().byte_length,
            },
            EffectiveEntrySummary::Step {
                node: NodeId(2),
                text_bytes: 36,
                reasoning_bytes: 42,
                tool_call_count: 1,
                tool_result_bytes: 12,
            },
        ],
        operated_nodes: vec![
            OperatedNode {
                node: NodeId(1),
                status: NodeStatus::Modified,
            },
            OperatedNode {
                node: NodeId(3),
                status: NodeStatus::Omitted,
            },
        ],
        channel_counts,
        total_text_bytes: 144,
        images: vec![image_ref_sample()],
        total_image_bytes: image_ref_sample().byte_length,
    }
}

pub(super) fn context_inspection_minimal() -> ContextInspection {
    ContextInspection {
        entries: Vec::new(),
        operated_nodes: Vec::new(),
        channel_counts: BTreeMap::new(),
        total_text_bytes: 0,
        images: Vec::new(),
        total_image_bytes: 0,
    }
}

enum AttemptOutcomeFixture {
    Sent,
    Ended {
        end: AttemptEnd,
        provider_error: Option<String>,
    },
}

struct AttemptFixture {
    origin: ReplayOrigin,
    usage: Option<TokenUsage>,
    usage_kind: UsageCompleteness,
    cost: Option<CostEstimate>,
    outcome: AttemptOutcomeFixture,
    retry_delay_ms: Option<u64>,
}

enum SlotStateFixture {
    Registered,
    Started,
    Terminal(StoredToolResult),
}

struct SlotFixture {
    invocation_id: lattice_conversation::InvocationId,
    binding: BindingIdentity,
    state: SlotStateFixture,
}

struct StepSpec {
    progress: StepProgress,
    attempts: Vec<AttemptFixture>,
    reply: Option<NativeMessage<BoundToolCall>>,
    slots: Vec<SlotFixture>,
    updated_at: Timestamp,
}

pub(super) fn session_record_at(current: NodeId, updated_at: Timestamp) -> SessionRecord {
    SessionRecord {
        session_id: session_id_primary(),
        project_path: project_path_non_utf8(),
        current,
        created_at: ts_created(),
        updated_at,
    }
}

fn binding_identity_for_call(
    call: &BoundToolCall,
    tool_identity: &str,
    arguments_digest_hex: &str,
) -> BindingIdentity {
    BindingIdentity {
        session_id: call.address.message.session_id,
        turn_id: call.address.message.turn_id,
        model_step_ordinal: call.address.message.model_step_ordinal,
        part_ordinal: call.address.part_ordinal,
        method: call.name.clone(),
        tool_identity: tool_identity.to_owned(),
        arguments_digest: hex_32(arguments_digest_hex),
    }
}

fn completed_attempt(usage: TokenUsage, cost: CostEstimate) -> AttemptFixture {
    AttemptFixture {
        origin: replay_origin_openai(),
        usage: Some(usage),
        usage_kind: UsageCompleteness::Complete,
        cost: Some(cost),
        outcome: AttemptOutcomeFixture::Ended {
            end: AttemptEnd::Completed,
            provider_error: None,
        },
        retry_delay_ms: None,
    }
}

fn sent_attempt_minimal() -> AttemptFixture {
    AttemptFixture {
        origin: replay_origin_openai(),
        usage: None,
        usage_kind: UsageCompleteness::Partial,
        cost: None,
        outcome: AttemptOutcomeFixture::Sent,
        retry_delay_ms: None,
    }
}

fn retryable_failed_attempt_full() -> AttemptFixture {
    AttemptFixture {
        origin: replay_origin_openai(),
        usage: Some(TokenUsage {
            input: Some(600),
            cache_read: Some(200),
            cache_write: Some(0),
            output: Some(40),
            reasoning: Some(15),
        }),
        usage_kind: UsageCompleteness::Partial,
        cost: Some(CostEstimate {
            usd: 0.0011,
            exact: false,
        }),
        outcome: AttemptOutcomeFixture::Ended {
            end: AttemptEnd::Stopped {
                cause: StopCause::ProviderError { retryable: true },
            },
            provider_error: Some("upstream 503 service unavailable".to_owned()),
        },
        retry_delay_ms: Some(500),
    }
}

/// 按 `docs/architecture/session/steps.md` 与 `recovery.md` 的对照表手写槽位投影。
fn hand_project_slot(slot: &SlotFixture) -> (SlotDetail, StoredToolResult) {
    let (progress, outcome, reason, material, stored_result) = match &slot.state {
        SlotStateFixture::Registered => (
            SlotState::Registered,
            ToolResultOutcome::NotExecuted,
            None,
            MaterialSummary::NoContent,
            StoredToolResult {
                invocation_id: slot.invocation_id,
                name: slot.binding.method.clone(),
                payload: ResultPayload::NotExecuted,
            },
        ),
        SlotStateFixture::Started => (
            SlotState::Started,
            ToolResultOutcome::Indeterminate,
            Some(SlotReason::Indeterminate(IndeterminateCause::NoResult)),
            MaterialSummary::NoContent,
            StoredToolResult {
                invocation_id: slot.invocation_id,
                name: slot.binding.method.clone(),
                payload: ResultPayload::Indeterminate(IndeterminateCause::NoResult),
            },
        ),
        SlotStateFixture::Terminal(result) => {
            assert_eq!(result.invocation_id, slot.invocation_id);
            assert_eq!(result.name, slot.binding.method);
            let (outcome, reason, material) = match &result.payload {
                ResultPayload::Succeeded(SuccessContent::Json {
                    digest,
                    byte_length,
                    ..
                }) => (
                    ToolResultOutcome::Succeeded,
                    None,
                    MaterialSummary::Available {
                        digest: *digest,
                        byte_length: *byte_length,
                    },
                ),
                ResultPayload::Succeeded(SuccessContent::OutputTooLarge {
                    digest,
                    byte_length,
                }) => (
                    ToolResultOutcome::Succeeded,
                    None,
                    MaterialSummary::OutputTooLarge {
                        digest: *digest,
                        byte_length: *byte_length,
                    },
                ),
                ResultPayload::Failed(err) => (
                    ToolResultOutcome::Failed,
                    Some(SlotReason::Failed {
                        code: err.code.clone(),
                    }),
                    MaterialSummary::NoContent,
                ),
                ResultPayload::Indeterminate(cause) => {
                    assert_ne!(
                        *cause,
                        IndeterminateCause::NoResult,
                        "持久 Terminal 槽位不得存放 NoResult（NoResult 由 Started 槽位表达）"
                    );
                    (
                        ToolResultOutcome::Indeterminate,
                        Some(SlotReason::Indeterminate(*cause)),
                        MaterialSummary::NoContent,
                    )
                }
                ResultPayload::NotExecuted => {
                    panic!("持久 Terminal 槽位不得存放 NotExecuted（未执行由 Registered 槽位表达）")
                }
            };
            (
                SlotState::Terminal {
                    outcome,
                    reason: reason.clone(),
                    material: material.clone(),
                },
                outcome,
                reason,
                material,
                result.clone(),
            )
        }
    };

    (
        SlotDetail {
            invocation_id: slot.invocation_id,
            binding: slot.binding.clone(),
            progress,
            outcome,
            reason,
            material,
        },
        stored_result,
    )
}

/// 按架构文档规则手写单次模型尝试的详情投影。
fn hand_project_attempt(
    attempt_ordinal: u32,
    record: &AttemptFixture,
    step_address: ModelMessageAddress,
) -> AttemptDetail {
    let (progress, end) = match &record.outcome {
        AttemptOutcomeFixture::Sent => (
            AttemptProgressDetail::Sent,
            AttemptEndDetail::Stopped {
                cause: StopCause::ProcessExit,
                diagnostic: None,
            },
        ),
        AttemptOutcomeFixture::Ended {
            end,
            provider_error,
        } => {
            let progress_detail = AttemptProgressDetail::Ended {
                end: end.clone(),
                diagnostic: provider_error.clone(),
            };
            let end_detail = match end {
                AttemptEnd::Completed => AttemptEndDetail::Completed { step: step_address },
                AttemptEnd::Stopped { cause } => AttemptEndDetail::Stopped {
                    cause: cause.clone(),
                    diagnostic: provider_error.clone(),
                },
            };
            (progress_detail, end_detail)
        }
    };

    AttemptDetail {
        attempt: attempt_ordinal,
        origin: record.origin.clone(),
        usage: record.usage,
        usage_kind: record.usage_kind,
        cost: record.cost,
        progress,
        end,
        retry_delay_ms: record.retry_delay_ms,
    }
}

/// 按 `recovery.md` 对照表确定 Step 的有效结束方式。
fn hand_effective_step_end(progress: &StepProgress, slots: &[SlotFixture]) -> StepEnd {
    match progress {
        StepProgress::Reserved | StepProgress::Requesting => StepEnd::Stop {
            cause: StopCause::ProcessExit,
        },
        StepProgress::Executing => {
            let slot_cause = slots.iter().find_map(|slot| match &slot.state {
                SlotStateFixture::Terminal(result) => match &result.payload {
                    ResultPayload::Indeterminate(IndeterminateCause::Cancelled) => {
                        Some(StopCause::Cancelled)
                    }
                    ResultPayload::Indeterminate(IndeterminateCause::Reported) => {
                        Some(StopCause::InvocationIndeterminate {
                            invocation_id: slot.invocation_id,
                        })
                    }
                    _ => None,
                },
                SlotStateFixture::Registered | SlotStateFixture::Started => None,
            });
            StepEnd::Stop {
                cause: slot_cause.unwrap_or(StopCause::ProcessExit),
            }
        }
        StepProgress::Closed { end } => end.clone(),
    }
}

/// 按 `steps.md` 与 `recovery.md` 手写 Step/Turn 结论摘要。
fn hand_turn_summary(
    progress: &StepProgress,
    attempts: &[AttemptFixture],
    slots: &[SlotFixture],
) -> TurnSummary {
    let end = hand_effective_step_end(progress, slots);
    match end {
        StepEnd::Finish => TurnSummary {
            outcome: PublicTurnOutcome::Completed,
            notice: None,
            progress: progress.clone(),
        },
        StepEnd::Continue => panic!("Continue 步骤不是轮次终态，不应生成 TurnSummary"),
        StepEnd::Stop { cause } => {
            if cause == StopCause::Cancelled {
                TurnSummary {
                    outcome: PublicTurnOutcome::Cancelled,
                    notice: Some(Notice {
                        level: NoticeLevel::Info,
                        text: "cancelled by caller".to_owned(),
                    }),
                    progress: progress.clone(),
                }
            } else {
                let text = match &cause {
                    StopCause::ProviderError { .. } => attempts
                        .last()
                        .and_then(|a| match &a.outcome {
                            AttemptOutcomeFixture::Ended { provider_error, .. } => {
                                provider_error.clone()
                            }
                            AttemptOutcomeFixture::Sent => None,
                        })
                        .expect("ProviderError 终止必须携带诊断文本"),
                    StopCause::Cancelled => unreachable!(),
                    StopCause::Deadline => "turn deadline exceeded".to_owned(),
                    StopCause::OutputLimit => "model output limit exceeded".to_owned(),
                    StopCause::InvalidBatch => "invalid tool call batch".to_owned(),
                    StopCause::ProcessExit => "process exited before turn closure".to_owned(),
                    StopCause::InvocationIndeterminate { .. } => {
                        "tool execution outcome is indeterminate".to_owned()
                    }
                    StopCause::ModelStepLimit => "model step limit exceeded".to_owned(),
                };
                TurnSummary {
                    outcome: PublicTurnOutcome::Failed,
                    notice: Some(Notice {
                        level: NoticeLevel::Error,
                        text,
                    }),
                    progress: progress.clone(),
                }
            }
        }
    }
}

/// 手写尝试用量汇总，规则与 `UsageSummary::from_attempts` 相同，但不调用它。
fn hand_usage_summary(attempts: &[&AttemptFixture]) -> UsageSummary {
    let count = attempts.len() as u64;
    let partial_attempts = attempts
        .iter()
        .filter(|a| a.usage_kind == UsageCompleteness::Partial)
        .count() as u64;

    let summarize_tokens = |pick: fn(&TokenUsage) -> Option<u64>| -> TokenSummary {
        let mut known = None;
        let mut complete = true;
        for attempt in attempts {
            match attempt.usage.as_ref().and_then(pick) {
                Some(value) => known = Some(known.unwrap_or(0) + value),
                None => complete = false,
            }
        }
        TokenSummary {
            known: if count == 0 { Some(0) } else { known },
            complete,
            overflowed: false,
        }
    };

    let mut cost: Option<CostEstimate> = None;
    let mut unknown_attempts = 0u64;
    for attempt in attempts {
        match attempt.cost {
            Some(value) => {
                let previous = cost.unwrap_or(CostEstimate {
                    usd: 0.0,
                    exact: true,
                });
                cost = Some(CostEstimate {
                    usd: previous.usd + value.usd,
                    exact: previous.exact && value.exact,
                });
            }
            None => unknown_attempts += 1,
        }
    }
    if count == 0 {
        cost = Some(CostEstimate {
            usd: 0.0,
            exact: true,
        });
    }
    if let Some(cost) = &mut cost {
        cost.exact &= unknown_attempts == 0;
    }

    UsageSummary {
        attempts: count,
        partial_attempts,
        kind: if partial_attempts > 0 {
            UsageCompleteness::Partial
        } else {
            UsageCompleteness::Complete
        },
        input: summarize_tokens(|u| u.input),
        cache_read: summarize_tokens(|u| u.cache_read),
        cache_write: summarize_tokens(|u| u.cache_write),
        output: summarize_tokens(|u| u.output),
        reasoning: summarize_tokens(|u| u.reasoning),
        cost: CostSummary {
            known: cost,
            unknown_attempts,
            overflowed: false,
        },
    }
}

fn build_scenario(spec: TurnSpec) -> BuiltScenario {
    assert!(!spec.steps.is_empty(), "真实 Turn 至少包含一个 Step");
    let input_node_id = NodeId(1);
    let start_node_id = NodeId(2);
    let input = ContextInput::new(channel_cli(), spec.user_parts).expect("固定用户输入应合法");

    let mut entries = Vec::with_capacity(1 + spec.steps.len());
    entries.push(ConversationEntry::Input {
        node: input_node_id,
        accepted_at: ts_accepted(),
        input,
    });

    let mut step_details = Vec::with_capacity(spec.steps.len());
    let mut all_attempts: Vec<&AttemptFixture> = Vec::new();
    let total_steps = spec.steps.len();

    for (idx, step_spec) in spec.steps.iter().enumerate() {
        let step_ordinal = idx as u32;
        let node_id = NodeId(start_node_id.0 + u64::from(step_ordinal));

        let is_continue = matches!(
            step_spec.progress,
            StepProgress::Closed {
                end: StepEnd::Continue
            }
        );
        assert_eq!(
            is_continue,
            idx + 1 < total_steps,
            "Continue 步骤必须且只能出现在非末尾步骤"
        );
        if matches!(
            step_spec.progress,
            StepProgress::Closed {
                end: StepEnd::Finish
            }
        ) {
            assert!(
                step_spec.slots.is_empty(),
                "以 Finish 结束的步骤不得携带工具调用槽位"
            );
        }
        if let Some(reply) = &step_spec.reply {
            let tool_call_count = reply.tool_calls().count();
            assert_eq!(
                tool_call_count,
                step_spec.slots.len(),
                "助手回复中的 ToolCall 部件数量必须与槽位数一致"
            );
        } else {
            assert!(
                step_spec.slots.is_empty(),
                "尚无助手回复的步骤不得存在已登记槽位"
            );
        }

        for attempt in &step_spec.attempts {
            all_attempts.push(attempt);
        }

        let step_address = ModelMessageAddress {
            session_id: spec.session_id,
            turn_id: spec.turn_id,
            model_step_ordinal: step_ordinal,
        };
        let attempt_details: Vec<AttemptDetail> = step_spec
            .attempts
            .iter()
            .enumerate()
            .map(|(attempt_idx, record)| {
                hand_project_attempt(attempt_idx as u32, record, step_address)
            })
            .collect();
        let mut slot_details = Vec::with_capacity(step_spec.slots.len());
        let mut stored_results = Vec::with_capacity(step_spec.slots.len());
        for slot in &step_spec.slots {
            let (detail, result) = hand_project_slot(slot);
            slot_details.push(detail);
            stored_results.push(result);
        }
        let effective_end = hand_effective_step_end(&step_spec.progress, &step_spec.slots);

        let step_detail = StepDetail {
            node: node_id,
            key: StepKey {
                turn_id: spec.turn_id,
                ordinal: step_ordinal,
            },
            progress: step_spec.progress.clone(),
            end: effective_end,
            attempts: attempt_details,
            slots: slot_details,
            updated_at: step_spec.updated_at,
        };

        let retries: Vec<RetryNotice> = step_spec
            .attempts
            .iter()
            .enumerate()
            .filter_map(|(attempt_idx, record)| {
                let delay_ms = record.retry_delay_ms?;
                let text = match &record.outcome {
                    AttemptOutcomeFixture::Ended { provider_error, .. } => provider_error
                        .clone()
                        .expect("带重试延迟的尝试必须包含 provider_error"),
                    AttemptOutcomeFixture::Sent => panic!("Sent 尝试不应携带 retry_delay_ms"),
                };
                Some(RetryNotice {
                    attempt: attempt_idx as u32,
                    delay_ms,
                    notice: Notice {
                        level: NoticeLevel::Warn,
                        text,
                    },
                })
            })
            .collect();

        let entry_summary = if is_continue {
            None
        } else {
            Some(hand_turn_summary(
                &step_spec.progress,
                &step_spec.attempts,
                &step_spec.slots,
            ))
        };

        let step_content = step_spec.reply.as_ref().map(|assistant| StepContent {
            assistant: assistant.clone(),
            results: stored_results,
        });

        entries.push(ConversationEntry::Step {
            node: node_id,
            step: step_detail.clone(),
            agent_name: (step_ordinal == 0).then(agent_coder),
            reply: step_content,
            retries,
            summary: entry_summary,
        });
        step_details.push(step_detail);
    }

    let last_step = spec.steps.last().expect("至少存在一个 Step");
    let turn_summary =
        hand_turn_summary(&last_step.progress, &last_step.attempts, &last_step.slots);
    let usage = hand_usage_summary(&all_attempts);
    let tip_node_id = NodeId(input_node_id.0 + total_steps as u64);

    let turn_detail = TurnDetail {
        turn_id: spec.turn_id,
        session_id: spec.session_id,
        start_node: start_node_id,
        input_nodes: vec![input_node_id],
        agent_name: agent_coder(),
        accepted_at: ts_accepted(),
        ended_at: last_step.updated_at,
        summary: turn_summary.clone(),
        usage: Box::new(usage.clone()),
        steps: step_details,
    };

    let unclosed_turn = (!last_step.progress.is_closed()).then_some(spec.turn_id);
    let mut log_rows = Vec::with_capacity(2 + total_steps);
    log_rows.push(LogRow {
        node: NodeHeader {
            id: NodeId(0),
            parent: None,
            turn_id: None,
            kind: NodeKind::Root,
        },
        current: false,
        turn_start: None,
    });
    log_rows.push(LogRow {
        node: NodeHeader {
            id: input_node_id,
            parent: Some(NodeId(0)),
            turn_id: None,
            kind: NodeKind::Input,
        },
        current: false,
        turn_start: None,
    });
    for ordinal in 0..total_steps {
        let id = NodeId(start_node_id.0 + ordinal as u64);
        log_rows.push(LogRow {
            node: NodeHeader {
                id,
                parent: Some(NodeId(id.0 - 1)),
                turn_id: Some(spec.turn_id),
                kind: NodeKind::Step {
                    ordinal: ordinal as u32,
                },
            },
            current: id == tip_node_id,
            turn_start: (ordinal == 0).then(|| TurnLogSummary {
                turn_id: spec.turn_id,
                summary: turn_summary.clone(),
                agent_name: agent_coder(),
            }),
        });
    }

    let session_log = SessionLog {
        session: session_record_at(tip_node_id, last_step.updated_at),
        current_agent: Some(agent_coder()),
        unclosed_turn,
        nodes: log_rows,
    };

    let session_usage = SessionUsage {
        session_id: spec.session_id,
        position: tip_node_id,
        turns: vec![TurnUsageRow {
            turn_id: spec.turn_id,
            start_node: start_node_id,
            provider: provider_openai(),
            model: model_gpt5(),
            usage: usage.clone(),
        }],
        path_total: usage.clone(),
        session_total: usage,
    };

    BuiltScenario {
        entries,
        turn_detail,
        session_log,
        session_usage,
    }
}

/// 场景 1：成功（Step 0 执行工具并以 `Continue` 关闭，`summary == None`；Step 1 以 `Finish` 关闭）。
pub(super) fn scenario_success_tool_then_finish() -> BuiltScenario {
    let call = bound_call_read_cargo(turn_id_1(), 0, 1, invocation_id_1(), Some("call_read_01"));
    let binding =
        binding_identity_for_call(&call, "builtin:read_file@v1", ARGS_CARGO_TOML_DIGEST_HEX);
    let slot = SlotFixture {
        invocation_id: call.invocation_id,
        binding,
        state: SlotStateFixture::Terminal(StoredToolResult {
            invocation_id: invocation_id_1(),
            name: "read_file".to_owned(),
            payload: ResultPayload::Succeeded(success_content_json_sample()),
        }),
    };
    let step_0_reply = NativeMessage {
        role: MessageRole::Assistant,
        provider_origin: Some(replay_origin_openai()),
        parts: vec![
            MessagePart {
                semantic: MessageSemantic::Reasoning {
                    text: "先读取 Cargo.toml 了解工作区成员".to_owned(),
                },
                replay_state: Some(opaque_state_sample()),
            },
            MessagePart {
                semantic: MessageSemantic::ToolCall { call },
                replay_state: None,
            },
        ],
    };
    let step_1_reply = NativeMessage {
        role: MessageRole::Assistant,
        provider_origin: Some(replay_origin_openai()),
        parts: vec![MessagePart {
            semantic: MessageSemantic::Text {
                text: "Cargo.toml 共包含 42 行配置。".to_owned(),
            },
            replay_state: None,
        }],
    };

    build_scenario(TurnSpec {
        session_id: session_id_primary(),
        turn_id: turn_id_1(),
        user_parts: user_text_parts("请查看 Cargo.toml 的行数"),
        steps: vec![
            StepSpec {
                progress: StepProgress::Closed {
                    end: StepEnd::Continue,
                },
                attempts: vec![completed_attempt(token_usage_full(), cost_estimate_exact())],
                reply: Some(step_0_reply),
                slots: vec![slot],
                updated_at: ts_step_0(),
            },
            StepSpec {
                progress: StepProgress::Closed {
                    end: StepEnd::Finish,
                },
                attempts: vec![completed_attempt(
                    TokenUsage {
                        input: Some(1550),
                        cache_read: Some(1200),
                        cache_write: Some(0),
                        output: Some(180),
                        reasoning: Some(40),
                    },
                    CostEstimate {
                        usd: 0.0028,
                        exact: true,
                    },
                )],
                reply: Some(step_1_reply),
                slots: Vec::new(),
                updated_at: ts_ended(),
            },
        ],
    })
}

/// 场景 2：模型请求失败后重试成功（Attempt 0 可重试错误带 `retry_delay_ms`，Attempt 1 成功关闭）。
pub(super) fn scenario_retry_then_success() -> BuiltScenario {
    let final_reply = NativeMessage {
        role: MessageRole::Assistant,
        provider_origin: Some(replay_origin_openai()),
        parts: vec![MessagePart {
            semantic: MessageSemantic::Text {
                text: "重试后已成功生成回复。".to_owned(),
            },
            replay_state: None,
        }],
    };
    build_scenario(TurnSpec {
        session_id: session_id_primary(),
        turn_id: turn_id_2(),
        user_parts: user_interleaved_image_parts("请结合截图检查服务连通性：", "定位报错原因"),
        steps: vec![StepSpec {
            progress: StepProgress::Closed {
                end: StepEnd::Finish,
            },
            attempts: vec![
                retryable_failed_attempt_full(),
                completed_attempt(token_usage_full(), cost_estimate_exact()),
            ],
            reply: Some(final_reply),
            slots: Vec::new(),
            updated_at: ts_ended(),
        }],
    })
}

/// 场景 3：工具失败（槽位状态与结果同为 `Failed(not_found)`，模型在下一步说明原因后结束）。
pub(super) fn scenario_tool_failed() -> BuiltScenario {
    let call = bound_call_read_missing(turn_id_3(), 0, 0, invocation_id_1());
    let binding =
        binding_identity_for_call(&call, "builtin:read_file@v1", ARGS_MISSING_TXT_DIGEST_HEX);
    let failed_slot = SlotFixture {
        invocation_id: call.invocation_id,
        binding,
        state: SlotStateFixture::Terminal(StoredToolResult {
            invocation_id: invocation_id_1(),
            name: "read_file".to_owned(),
            payload: ResultPayload::Failed(tool_error_not_found()),
        }),
    };
    let step_0_reply = NativeMessage {
        role: MessageRole::Assistant,
        provider_origin: Some(replay_origin_openai()),
        parts: vec![MessagePart {
            semantic: MessageSemantic::ToolCall { call },
            replay_state: None,
        }],
    };
    let step_1_reply = NativeMessage {
        role: MessageRole::Assistant,
        provider_origin: Some(replay_origin_openai()),
        parts: vec![MessagePart {
            semantic: MessageSemantic::Text {
                text: "目标文件 missing.txt 不存在。".to_owned(),
            },
            replay_state: None,
        }],
    };

    build_scenario(TurnSpec {
        session_id: session_id_primary(),
        turn_id: turn_id_3(),
        user_parts: user_text_parts("读取 missing.txt"),
        steps: vec![
            StepSpec {
                progress: StepProgress::Closed {
                    end: StepEnd::Continue,
                },
                attempts: vec![completed_attempt(token_usage_full(), cost_estimate_exact())],
                reply: Some(step_0_reply),
                slots: vec![failed_slot],
                updated_at: ts_step_0(),
            },
            StepSpec {
                progress: StepProgress::Closed {
                    end: StepEnd::Finish,
                },
                attempts: vec![completed_attempt(token_usage_full(), cost_estimate_exact())],
                reply: Some(step_1_reply),
                slots: Vec::new(),
                updated_at: ts_ended(),
            },
        ],
    })
}

/// 场景 4：用户取消（槽位 0 取消终止、槽位 1 仍为 `Registered`/`NotExecuted`，Step 以 `Stop { Cancelled }` 关闭）。
pub(super) fn scenario_cancelled() -> BuiltScenario {
    let call_0 = bound_call_sleep_cmd(turn_id_1(), 0, 0, invocation_id_1());
    let call_1 = bound_call_read_cargo(turn_id_1(), 0, 1, invocation_id_2(), None);
    let slot_0 = SlotFixture {
        invocation_id: call_0.invocation_id,
        binding: binding_identity_for_call(
            &call_0,
            "builtin:run_shell@v1",
            ARGS_SLEEP_CMD_DIGEST_HEX,
        ),
        state: SlotStateFixture::Terminal(StoredToolResult {
            invocation_id: invocation_id_1(),
            name: "run_shell".to_owned(),
            payload: ResultPayload::Indeterminate(IndeterminateCause::Cancelled),
        }),
    };
    let slot_1 = SlotFixture {
        invocation_id: call_1.invocation_id,
        binding: binding_identity_for_call(
            &call_1,
            "builtin:read_file@v1",
            ARGS_CARGO_TOML_DIGEST_HEX,
        ),
        state: SlotStateFixture::Registered,
    };
    let reply = NativeMessage {
        role: MessageRole::Assistant,
        provider_origin: Some(replay_origin_openai()),
        parts: vec![
            MessagePart {
                semantic: MessageSemantic::ToolCall { call: call_0 },
                replay_state: None,
            },
            MessagePart {
                semantic: MessageSemantic::ToolCall { call: call_1 },
                replay_state: None,
            },
        ],
    };

    build_scenario(TurnSpec {
        session_id: session_id_primary(),
        turn_id: turn_id_1(),
        user_parts: user_text_parts("运行耗时命令后读取配置"),
        steps: vec![StepSpec {
            progress: StepProgress::Closed {
                end: StepEnd::Stop {
                    cause: StopCause::Cancelled,
                },
            },
            attempts: vec![completed_attempt(token_usage_full(), cost_estimate_exact())],
            reply: Some(reply),
            slots: vec![slot_0, slot_1],
            updated_at: ts_ended(),
        }],
    })
}

/// 场景 5：执行者消失——停在 `Reserved`（尚未发起模型请求）。
pub(super) fn scenario_orphan_reserved() -> BuiltScenario {
    build_scenario(TurnSpec {
        session_id: session_id_primary(),
        turn_id: turn_id_2(),
        user_parts: user_text_parts("刚接受尚未发出请求的轮次"),
        steps: vec![StepSpec {
            progress: StepProgress::Reserved,
            attempts: Vec::new(),
            reply: None,
            slots: Vec::new(),
            updated_at: ts_accepted(),
        }],
    })
}

/// 场景 6：执行者消失——停在 `Requesting`（模型请求 `Sent` 在途）。
pub(super) fn scenario_orphan_requesting() -> BuiltScenario {
    build_scenario(TurnSpec {
        session_id: session_id_primary(),
        turn_id: turn_id_2(),
        user_parts: user_text_parts("模型流式响应中途进程退出"),
        steps: vec![StepSpec {
            progress: StepProgress::Requesting,
            attempts: vec![sent_attempt_minimal()],
            reply: None,
            slots: Vec::new(),
            updated_at: ts_step_0(),
        }],
    })
}

/// 场景 7：执行者消失——停在 `Executing`（槽位 0 已完成，槽位 1 仍停在 `Registered`）。
pub(super) fn scenario_orphan_executing() -> BuiltScenario {
    let call_0 = bound_call_read_cargo(turn_id_2(), 0, 0, invocation_id_1(), Some("call_exec_0"));
    let call_1 = bound_call_read_missing(turn_id_2(), 0, 1, invocation_id_2());
    let slot_0 = SlotFixture {
        invocation_id: call_0.invocation_id,
        binding: binding_identity_for_call(
            &call_0,
            "builtin:read_file@v1",
            ARGS_CARGO_TOML_DIGEST_HEX,
        ),
        state: SlotStateFixture::Terminal(StoredToolResult {
            invocation_id: invocation_id_1(),
            name: "read_file".to_owned(),
            payload: ResultPayload::Succeeded(success_content_json_sample()),
        }),
    };
    let slot_1 = SlotFixture {
        invocation_id: call_1.invocation_id,
        binding: binding_identity_for_call(
            &call_1,
            "builtin:read_file@v1",
            ARGS_MISSING_TXT_DIGEST_HEX,
        ),
        state: SlotStateFixture::Registered,
    };
    let reply = NativeMessage {
        role: MessageRole::Assistant,
        provider_origin: Some(replay_origin_openai()),
        parts: vec![
            MessagePart {
                semantic: MessageSemantic::ToolCall { call: call_0 },
                replay_state: None,
            },
            MessagePart {
                semantic: MessageSemantic::ToolCall { call: call_1 },
                replay_state: None,
            },
        ],
    };

    build_scenario(TurnSpec {
        session_id: session_id_primary(),
        turn_id: turn_id_2(),
        user_parts: user_text_parts("依次读取两个文件"),
        steps: vec![StepSpec {
            progress: StepProgress::Executing,
            attempts: vec![completed_attempt(token_usage_full(), cost_estimate_exact())],
            reply: Some(reply),
            slots: vec![slot_0, slot_1],
            updated_at: ts_step_0(),
        }],
    })
}

/// 场景 8：调用效果不确定——`Indeterminate(NoResult)`（槽位已越过 `Started`，轮次超时停止时尚未取得结果）。
pub(super) fn scenario_indeterminate_no_result() -> BuiltScenario {
    let call = bound_call_sleep_cmd(turn_id_3(), 0, 0, invocation_id_1());
    let started_slot = SlotFixture {
        invocation_id: call.invocation_id,
        binding: binding_identity_for_call(
            &call,
            "builtin:run_shell@v1",
            ARGS_SLEEP_CMD_DIGEST_HEX,
        ),
        state: SlotStateFixture::Started,
    };
    let reply = NativeMessage {
        role: MessageRole::Assistant,
        provider_origin: Some(replay_origin_openai()),
        parts: vec![MessagePart {
            semantic: MessageSemantic::ToolCall { call },
            replay_state: None,
        }],
    };

    build_scenario(TurnSpec {
        session_id: session_id_primary(),
        turn_id: turn_id_3(),
        user_parts: user_text_parts("执行外部脚本直至轮次超时"),
        steps: vec![StepSpec {
            progress: StepProgress::Closed {
                end: StepEnd::Stop {
                    cause: StopCause::Deadline,
                },
            },
            attempts: vec![completed_attempt(token_usage_full(), cost_estimate_exact())],
            reply: Some(reply),
            slots: vec![started_slot],
            updated_at: ts_ended(),
        }],
    })
}

pub(super) fn store_session_list_row_full() -> StoreSessionListRow {
    StoreSessionListRow {
        session_id: session_id_primary(),
        project_path: project_path_non_utf8(),
        current: NodeId(2),
        node_count: 3,
        unclosed_turn: Some(turn_id_2()),
        updated_at: ts_step_0(),
    }
}

pub(super) fn store_session_list_row_minimal() -> StoreSessionListRow {
    StoreSessionListRow {
        session_id: session_id_primary(),
        project_path: project_path_non_utf8(),
        current: NodeId(0),
        node_count: 1,
        unclosed_turn: None,
        updated_at: ts_created(),
    }
}

pub(super) fn register(book: &mut CaseBook) {
    let success = scenario_success_tool_then_finish();
    let retried = scenario_retry_then_success();
    let tool_failed = scenario_tool_failed();
    let orphan_reserved = scenario_orphan_reserved();
    let orphan_requesting = scenario_orphan_requesting();
    let indeterminate = scenario_indeterminate_no_result();

    enum_fragments!(
        book,
        "node_kind",
        NodeKind {
            ("root", NodeKind::Root => NodeKind::Root),
            ("input", NodeKind::Input => NodeKind::Input),
            ("context_op", NodeKind::ContextOp => NodeKind::ContextOp),
            ("step", NodeKind::Step { .. } => NodeKind::Step { ordinal: 0 }),
        }
    );

    enum_fragments!(
        book,
        "path_node_kind",
        PathNodeKind {
            ("input", PathNodeKind::Input => PathNodeKind::Input),
            ("step", PathNodeKind::Step => PathNodeKind::Step),
            ("op", PathNodeKind::Op => PathNodeKind::Op),
        }
    );

    enum_fragments!(
        book,
        "node_status",
        NodeStatus {
            ("modified", NodeStatus::Modified => NodeStatus::Modified),
            ("skipped_empty", NodeStatus::SkippedEmpty => NodeStatus::SkippedEmpty),
            ("omitted", NodeStatus::Omitted => NodeStatus::Omitted),
        }
    );

    enum_fragments!(
        book,
        "effective_entry_summary",
        EffectiveEntrySummary {
            (
                "input",
                EffectiveEntrySummary::Input { .. } => EffectiveEntrySummary::Input {
                    node: NodeId(1),
                    channel: channel_cli(),
                    text_bytes: 54,
                    image_count: 1,
                    image_bytes: image_ref_sample().byte_length,
                }
            ),
            (
                "step",
                EffectiveEntrySummary::Step { .. } => EffectiveEntrySummary::Step {
                    node: NodeId(2),
                    text_bytes: 36,
                    reasoning_bytes: 42,
                    tool_call_count: 1,
                    tool_result_bytes: 12,
                }
            ),
        }
    );

    book.insert(
        "fragment/operated_node/modified",
        &OperatedNode {
            node: NodeId(1),
            status: NodeStatus::Modified,
        },
    );
    book.insert_struct_pair(
        "context_inspection",
        &context_inspection_full(),
        &context_inspection_minimal(),
    );

    enum_fragments!(
        book,
        "context_edit",
        ContextEdit {
            ("omit_node", ContextEdit::OmitNode { .. } => ContextEdit::omit_node(NodeId(1))),
            (
                "replace_input",
                ContextEdit::ReplaceInput { .. } => ContextEdit::replace_input(
                    NodeId(1),
                    user_interleaved_image_parts("替换前文本", "替换后文本"),
                )
                .expect("固定 ReplaceInput 编辑应合法")
            ),
            (
                "replace_step",
                ContextEdit::ReplaceStep { .. } => ContextEdit::replace_step(
                    NodeId(2),
                    "精简后的助手结论",
                )
                .expect("固定 ReplaceStep 编辑应合法")
            ),
            (
                "omit_invocation",
                ContextEdit::OmitInvocation { .. } => ContextEdit::omit_invocation(invocation_id_1())
            ),
            (
                "replace_tool_result",
                ContextEdit::ReplaceToolResult { .. } => ContextEdit::replace_tool_result(
                    invocation_id_1(),
                    "{\"lines\":42}",
                )
                .expect("固定 ReplaceToolResult 编辑应合法")
            ),
            (
                "strip_reasoning",
                ContextEdit::StripReasoning { .. } => ContextEdit::strip_reasoning(NodeId(2))
            ),
            (
                "replace_image",
                ContextEdit::ReplaceImage { .. } => ContextEdit::replace_image(
                    NodeId(1),
                    image_ref_sample(),
                    Some("[图片摘要：状态面板正常]".to_owned()),
                )
                .expect("固定 ReplaceImage 编辑应合法")
            ),
        }
    );
    book.insert(
        "fragment/context_edit/replace_image_without_text",
        &ContextEdit::replace_image(NodeId(1), image_ref_sample(), None)
            .expect("无替代文本的 ReplaceImage 编辑应合法"),
    );

    let context_input_full = ContextInput::new(
        channel_cli(),
        user_interleaved_image_parts("请结合截图检查服务连通性：", "定位报错原因"),
    )
    .expect("固定多模态 ContextInput 应合法");
    let context_input_minimal =
        ContextInput::new(channel_tui(), user_text_parts("请查看 Cargo.toml 的行数"))
            .expect("固定纯文本 ContextInput 应合法");
    book.insert_struct_pair("context_input", &context_input_full, &context_input_minimal);

    let context_op_entry = sample_context_op_entry();
    if let ConversationEntry::ContextOp { op, .. } = &context_op_entry {
        book.insert("fragment/context_op/full", op);
    }

    enum_fragments!(
        book,
        "public_turn_outcome",
        PublicTurnOutcome {
            ("completed", PublicTurnOutcome::Completed => PublicTurnOutcome::Completed),
            ("failed", PublicTurnOutcome::Failed => PublicTurnOutcome::Failed),
            ("cancelled", PublicTurnOutcome::Cancelled => PublicTurnOutcome::Cancelled),
        }
    );

    enum_fragments!(
        book,
        "notice_level",
        NoticeLevel {
            ("info", NoticeLevel::Info => NoticeLevel::Info),
            ("warn", NoticeLevel::Warn => NoticeLevel::Warn),
            ("error", NoticeLevel::Error => NoticeLevel::Error),
        }
    );

    enum_fragments!(
        book,
        "stop_cause",
        StopCause {
            (
                "provider_error",
                StopCause::ProviderError { .. } => StopCause::ProviderError { retryable: false }
            ),
            ("cancelled", StopCause::Cancelled => StopCause::Cancelled),
            ("deadline", StopCause::Deadline => StopCause::Deadline),
            ("output_limit", StopCause::OutputLimit => StopCause::OutputLimit),
            ("invalid_batch", StopCause::InvalidBatch => StopCause::InvalidBatch),
            ("process_exit", StopCause::ProcessExit => StopCause::ProcessExit),
            (
                "invocation_indeterminate",
                StopCause::InvocationIndeterminate { .. } => StopCause::InvocationIndeterminate {
                    invocation_id: invocation_id_1(),
                }
            ),
            ("model_step_limit", StopCause::ModelStepLimit => StopCause::ModelStepLimit),
        }
    );

    enum_fragments!(
        book,
        "step_end",
        StepEnd {
            ("continue", StepEnd::Continue => StepEnd::Continue),
            ("finish", StepEnd::Finish => StepEnd::Finish),
            (
                "stop",
                StepEnd::Stop { .. } => StepEnd::Stop {
                    cause: StopCause::Cancelled,
                }
            ),
        }
    );

    enum_fragments!(
        book,
        "step_progress",
        StepProgress {
            ("reserved", StepProgress::Reserved => StepProgress::Reserved),
            ("requesting", StepProgress::Requesting => StepProgress::Requesting),
            ("executing", StepProgress::Executing => StepProgress::Executing),
            (
                "closed",
                StepProgress::Closed { .. } => StepProgress::Closed {
                    end: StepEnd::Finish,
                }
            ),
        }
    );

    enum_fragments!(
        book,
        "attempt_end",
        AttemptEnd {
            ("completed", AttemptEnd::Completed => AttemptEnd::Completed),
            (
                "stopped",
                AttemptEnd::Stopped { .. } => AttemptEnd::Stopped {
                    cause: StopCause::ProviderError { retryable: true },
                }
            ),
        }
    );

    let step_address = ModelMessageAddress {
        session_id: session_id_primary(),
        turn_id: turn_id_1(),
        model_step_ordinal: 0,
    };

    enum_fragments!(
        book,
        "attempt_progress_detail",
        AttemptProgressDetail {
            ("sent", AttemptProgressDetail::Sent => AttemptProgressDetail::Sent),
            (
                "ended",
                AttemptProgressDetail::Ended { .. } => AttemptProgressDetail::Ended {
                    end: AttemptEnd::Stopped {
                        cause: StopCause::ProviderError { retryable: true },
                    },
                    diagnostic: Some("upstream 503 service unavailable".to_owned()),
                }
            ),
        }
    );

    enum_fragments!(
        book,
        "attempt_end_detail",
        AttemptEndDetail {
            (
                "completed",
                AttemptEndDetail::Completed { .. } => AttemptEndDetail::Completed {
                    step: step_address,
                }
            ),
            (
                "stopped",
                AttemptEndDetail::Stopped { .. } => AttemptEndDetail::Stopped {
                    cause: StopCause::ProcessExit,
                    diagnostic: None,
                }
            ),
        }
    );

    enum_fragments!(
        book,
        "slot_reason",
        SlotReason {
            (
                "failed",
                SlotReason::Failed { .. } => SlotReason::Failed {
                    code: "not_found".to_owned(),
                }
            ),
            (
                "indeterminate",
                SlotReason::Indeterminate(_) => SlotReason::Indeterminate(IndeterminateCause::NoResult)
            ),
        }
    );

    enum_fragments!(
        book,
        "material_summary",
        MaterialSummary {
            (
                "available",
                MaterialSummary::Available { .. } => MaterialSummary::Available {
                    digest: hex_32(CONTENT_LINES_42_DIGEST_HEX),
                    byte_length: CONTENT_LINES_42_BYTES,
                }
            ),
            (
                "output_too_large",
                MaterialSummary::OutputTooLarge { .. } => MaterialSummary::OutputTooLarge {
                    digest: hex_32(CONTENT_OVERSIZE_DIGEST_HEX),
                    byte_length: CONTENT_OVERSIZE_BYTES,
                }
            ),
            ("no_content", MaterialSummary::NoContent => MaterialSummary::NoContent),
        }
    );

    enum_fragments!(
        book,
        "slot_state",
        SlotState {
            ("registered", SlotState::Registered => SlotState::Registered),
            ("started", SlotState::Started => SlotState::Started),
            (
                "terminal",
                SlotState::Terminal { .. } => SlotState::Terminal {
                    outcome: ToolResultOutcome::Succeeded,
                    reason: None,
                    material: MaterialSummary::Available {
                        digest: hex_32(CONTENT_LINES_42_DIGEST_HEX),
                        byte_length: CONTENT_LINES_42_BYTES,
                    },
                }
            ),
        }
    );

    // ConversationEntry::Step 主片段取 Step 0（agent_name 为 Some("coder")、reply 为 Some、summary 为 None），
    // 并补充 step_continuation（Step 1：agent_name 为 None、summary 为 Some）与 step_reserved（reply 为 None）。
    enum_fragments!(
        book,
        "conversation_entry",
        ConversationEntry {
            ("input", ConversationEntry::Input { .. } => success.entries[0].clone()),
            ("context_op", ConversationEntry::ContextOp { .. } => context_op_entry.clone()),
            ("step", ConversationEntry::Step { .. } => success.entries[1].clone()),
        }
    );
    book.insert(
        "fragment/conversation_entry/input_with_image",
        &retried.entries[0],
    );
    book.insert(
        "fragment/conversation_entry/step_continuation",
        &success.entries[2],
    );
    book.insert(
        "fragment/conversation_entry/step_reserved",
        &orphan_reserved.entries[1],
    );

    let turn_summary_full = indeterminate.turn_detail.summary.clone();
    let turn_summary_minimal = success.turn_detail.summary.clone();
    book.insert_struct_pair("turn_summary", &turn_summary_full, &turn_summary_minimal);

    book.insert_struct_pair(
        "attempt_detail",
        &retried.turn_detail.steps[0].attempts[0],
        &orphan_requesting.turn_detail.steps[0].attempts[0],
    );

    book.insert_struct_pair(
        "slot_detail",
        &tool_failed.turn_detail.steps[0].slots[0],
        &success.turn_detail.steps[0].slots[0],
    );

    book.insert_struct_pair(
        "step_detail",
        &success.turn_detail.steps[0],
        &orphan_reserved.turn_detail.steps[0],
    );

    book.insert_struct_pair(
        "turn_detail",
        &success.turn_detail,
        &orphan_reserved.turn_detail,
    );

    book.insert_struct_pair(
        "store_session_list_row",
        &store_session_list_row_full(),
        &store_session_list_row_minimal(),
    );

    let node_header_full = success.session_log.nodes[2].node.clone();
    let node_header_minimal = success.session_log.nodes[0].node.clone();
    book.insert_struct_pair("node_header", &node_header_full, &node_header_minimal);

    let log_row_full = success.session_log.nodes[2].clone();
    let log_row_minimal = success.session_log.nodes[0].clone();
    book.insert_struct_pair("log_row", &log_row_full, &log_row_minimal);

    let session_log_full = orphan_requesting.session_log;
    let session_log_minimal = SessionLog {
        session: session_record_at(NodeId(0), ts_created()),
        current_agent: Some(agent_coder()),
        unclosed_turn: None,
        nodes: vec![LogRow {
            node: NodeHeader {
                id: NodeId(0),
                parent: None,
                turn_id: None,
                kind: NodeKind::Root,
            },
            current: true,
            turn_start: None,
        }],
    };
    book.insert_struct_pair("session_log", &session_log_full, &session_log_minimal);

    let empty_usage = hand_usage_summary(&[]);
    let session_usage_minimal = SessionUsage {
        session_id: session_id_primary(),
        position: NodeId(0),
        turns: Vec::new(),
        path_total: empty_usage.clone(),
        session_total: empty_usage,
    };
    book.insert_struct_pair(
        "session_usage",
        &success.session_usage,
        &session_usage_minimal,
    );
}
