// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `lattice-session` 线上类型片段与按架构文档规则手写的内部一致场景构造器。
//!
//! 同一份场景输入同时生成原始持久节点（`Node`、`ConversationEntry`）与查询详情
//! （`TurnDetail`、`StepDetail`、`AttemptDetail`、`SlotDetail`、`SessionLog`、`SessionUsage`），
//! 不调用生产派生函数（如 `resolve_*`、`UsageSummary::from_attempts`）或摘要函数。

use lattice_conversation::{
    BoundToolCall, CostEstimate, CostSummary, Effort, IndeterminateCause, MessagePart, MessageRole,
    MessageSemantic, ModelMessageAddress, NativeMessage, ResultPayload, SessionId,
    StoredToolResult, SuccessContent, TokenSummary, TokenUsage, ToolCallValidation,
    ToolResultOutcome, TurnId, UsageCompleteness, UsageSummary,
};
use lattice_session::store::{
    AttemptDetail, AttemptDiagnostic, AttemptEnd, AttemptEndDetail, AttemptProgress,
    AttemptProgressDetail, AttemptRecord, BindingIdentity, ConversationEntry, InvocationBinding,
    InvocationSlot, LogRow, MAX_MODEL_STEPS, MaterialSummary, Node, NodeHeader, NodeId, NodeKind,
    Notice, NoticeLevel, PublicTurnOutcome, RetryNotice, SessionListRow as StoreSessionListRow,
    SessionLog, SessionRecord, SessionUsage, SlotDetail, SlotProgress, SlotReason, SlotState,
    StepDetail, StepEnd, StepKey, StepProgress, StopCause, Timestamp, TurnDetail,
    TurnExecutionLimits, TurnLogSummary, TurnSummary, TurnUsageRow,
};

use super::{
    ARGS_CARGO_TOML_DIGEST_HEX, ARGS_MISSING_TXT_DIGEST_HEX, ARGS_SLEEP_CMD_DIGEST_HEX,
    CONFIG_DIGEST_HEX, CONTENT_LINES_42_BYTES, CONTENT_LINES_42_DIGEST_HEX, CONTENT_OVERSIZE_BYTES,
    CONTENT_OVERSIZE_DIGEST_HEX, CaseBook, agent_coder,
    conversation::{
        bound_call_read_cargo, bound_call_read_missing, bound_call_sleep_cmd, cost_estimate_exact,
        opaque_state_sample, replay_origin_openai, success_content_json_sample, token_usage_full,
        tool_error_not_found,
    },
    hex_32, invocation_id_1, invocation_id_2, model_gpt5, project_path_non_utf8, provider_openai,
    session_id_primary, ts_accepted, ts_created, ts_ended, ts_step_0, turn_id_1, turn_id_2,
    turn_id_3, turn_timeout_120s,
};

/// 单个场景从同一份输入同时产出的原始节点与投影视图。
pub(super) struct BuiltScenario {
    pub(super) entries: Vec<ConversationEntry>,
    pub(super) turn_detail: TurnDetail,
    pub(super) session_log: SessionLog,
    pub(super) session_usage: SessionUsage,
}

struct TurnSpec {
    session_id: SessionId,
    turn_id: TurnId,
    turn_ordinal: u64,
    effort: Option<Effort>,
    user_prompt: &'static str,
    steps: Vec<StepSpec>,
}

struct StepSpec {
    progress: StepProgress,
    attempts: Vec<AttemptRecord>,
    reply: Option<NativeMessage<BoundToolCall>>,
    slots: Vec<InvocationSlot>,
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

fn turn_limits() -> TurnExecutionLimits {
    TurnExecutionLimits {
        timeout_secs: turn_timeout_120s(),
        max_model_steps: MAX_MODEL_STEPS,
        max_model_attempts: 3,
    }
}

fn binding_for_call(
    call: &BoundToolCall,
    tool_identity: &str,
    arguments_digest_hex: &str,
) -> InvocationBinding {
    InvocationBinding {
        invocation_id: call.invocation_id,
        identity: BindingIdentity {
            session_id: call.address.message.session_id,
            turn_id: call.address.message.turn_id,
            model_step_ordinal: call.address.message.model_step_ordinal,
            part_ordinal: call.address.part_ordinal,
            method: call.name.clone(),
            tool_identity: tool_identity.to_owned(),
            arguments_digest: hex_32(arguments_digest_hex),
        },
    }
}

fn completed_attempt(usage: TokenUsage, cost: CostEstimate) -> AttemptRecord {
    AttemptRecord {
        origin: replay_origin_openai(),
        usage: Some(usage),
        usage_kind: UsageCompleteness::Complete,
        cost: Some(cost),
        progress: AttemptProgress::Ended {
            end: AttemptEnd::Completed,
            diagnostic: AttemptDiagnostic {
                provider_error: None,
                invalid_batch: None,
            },
        },
        retry_delay_ms: None,
    }
}

fn sent_attempt_minimal() -> AttemptRecord {
    AttemptRecord {
        origin: replay_origin_openai(),
        usage: None,
        usage_kind: UsageCompleteness::Partial,
        cost: None,
        progress: AttemptProgress::Sent,
        retry_delay_ms: None,
    }
}

fn retryable_failed_attempt_full() -> AttemptRecord {
    AttemptRecord {
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
        progress: AttemptProgress::Ended {
            end: AttemptEnd::Stopped {
                cause: StopCause::ProviderError { retryable: true },
            },
            diagnostic: AttemptDiagnostic {
                provider_error: Some("upstream 503 service unavailable".to_owned()),
                invalid_batch: None,
            },
        },
        retry_delay_ms: Some(500),
    }
}

/// 按 `docs/architecture/session/steps.md` 与 `recovery.md` 的对照表手写槽位投影。
fn hand_project_slot(slot: &InvocationSlot) -> SlotDetail {
    let (progress, outcome, reason, material) = match &slot.progress {
        SlotProgress::Registered => (
            SlotState::Registered,
            ToolResultOutcome::NotExecuted,
            None,
            MaterialSummary::NoContent,
        ),
        SlotProgress::Started => (
            SlotState::Started,
            ToolResultOutcome::Indeterminate,
            Some(SlotReason::Indeterminate(IndeterminateCause::NoResult)),
            MaterialSummary::NoContent,
        ),
        SlotProgress::Terminal { result } => {
            assert_eq!(result.invocation_id, slot.binding.invocation_id);
            assert_eq!(result.name, slot.binding.identity.method);
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
            )
        }
    };

    SlotDetail {
        invocation_id: slot.binding.invocation_id,
        binding: slot.binding.identity.clone(),
        progress,
        outcome,
        reason,
        material,
    }
}

/// 按架构文档规则手写单次模型尝试的详情投影。
fn hand_project_attempt(
    attempt_ordinal: u32,
    record: &AttemptRecord,
    step_address: ModelMessageAddress,
) -> AttemptDetail {
    let (progress, end) = match &record.progress {
        AttemptProgress::Sent => (
            AttemptProgressDetail::Sent,
            AttemptEndDetail::Stopped {
                cause: StopCause::ProcessExit,
                diagnostic: None,
            },
        ),
        AttemptProgress::Ended { end, diagnostic } => {
            let progress_detail = AttemptProgressDetail::Ended {
                end: end.clone(),
                diagnostic: diagnostic.provider_error.clone(),
            };
            let end_detail = match end {
                AttemptEnd::Completed => AttemptEndDetail::Completed { step: step_address },
                AttemptEnd::Stopped { cause } => AttemptEndDetail::Stopped {
                    cause: cause.clone(),
                    diagnostic: diagnostic.provider_error.clone(),
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
fn hand_effective_step_end(progress: &StepProgress, slots: &[InvocationSlot]) -> StepEnd {
    match progress {
        StepProgress::Reserved | StepProgress::Requesting => StepEnd::Stop {
            cause: StopCause::ProcessExit,
        },
        StepProgress::Executing => {
            let slot_cause = slots.iter().find_map(|slot| match &slot.progress {
                SlotProgress::Terminal { result } => match &result.payload {
                    ResultPayload::Indeterminate(IndeterminateCause::Cancelled) => {
                        Some(StopCause::Cancelled)
                    }
                    ResultPayload::Indeterminate(IndeterminateCause::Reported) => {
                        Some(StopCause::InvocationIndeterminate {
                            invocation_id: slot.binding.invocation_id,
                        })
                    }
                    _ => None,
                },
                SlotProgress::Registered | SlotProgress::Started => None,
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
    attempts: &[AttemptRecord],
    slots: &[InvocationSlot],
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
                        .and_then(|a| match &a.progress {
                            AttemptProgress::Ended { diagnostic, .. } => {
                                diagnostic.provider_error.clone()
                            }
                            AttemptProgress::Sent => None,
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

/// 手写尝试用量汇总，规则与 `UsageSummary::from_attempts` 相同，但不调用它：
/// - 各项计数分别累加；某次尝试没有报告这一项，这一项就不完整；
/// - 没有尝试时，各项计数为 0 且完整，费用为精确的 0；
/// - 费用按尝试顺序以 `f64` 依次相加，不做舍入；有费用未知的尝试时不精确；
/// - 有部分尝试时整体为 partial。夹具的计数都很小，不涉及溢出。
fn hand_usage_summary(attempts: &[&AttemptRecord]) -> UsageSummary {
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
    let user_node_id = NodeId(1);
    let user_node = Node::UserMessage {
        parent: NodeId(0),
        turn_id: spec.turn_id,
        accepted_at: ts_accepted(),
        agent_name: agent_coder(),
        model: model_gpt5(),
        provider: provider_openai(),
        effort: spec.effort,
        limits: turn_limits(),
        config_digest: hex_32(CONFIG_DIGEST_HEX),
        message: NativeMessage {
            role: MessageRole::User,
            provider_origin: None,
            parts: vec![MessagePart {
                semantic: MessageSemantic::Text {
                    text: spec.user_prompt.to_owned(),
                },
                replay_state: None,
            }],
        },
    };

    let mut entries = Vec::with_capacity(1 + spec.steps.len());
    entries.push(ConversationEntry::UserMessage {
        node: user_node_id,
        content: user_node,
        ordinal: spec.turn_ordinal,
    });

    let mut step_details = Vec::with_capacity(spec.steps.len());
    let mut all_attempts: Vec<&AttemptRecord> = Vec::new();
    let total_steps = spec.steps.len();

    for (idx, step_spec) in spec.steps.iter().enumerate() {
        let step_ordinal = idx as u32;
        let node_id = NodeId(user_node_id.0 + 1 + u64::from(step_ordinal));
        let parent_id = NodeId(node_id.0 - 1);

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
        let slot_details: Vec<SlotDetail> = step_spec.slots.iter().map(hand_project_slot).collect();
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
                let text = match &record.progress {
                    AttemptProgress::Ended { diagnostic, .. } => diagnostic
                        .provider_error
                        .clone()
                        .expect("带重试延迟的尝试必须包含 provider_error"),
                    AttemptProgress::Sent => panic!("Sent 尝试不应携带 retry_delay_ms"),
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

        let raw_step_node = Node::Step {
            parent: parent_id,
            turn_id: spec.turn_id,
            step_ordinal,
            progress: step_spec.progress.clone(),
            attempts: step_spec.attempts.clone(),
            reply: step_spec.reply.clone(),
            slots: step_spec.slots.clone(),
            updated_at: step_spec.updated_at,
        };

        entries.push(ConversationEntry::Step {
            node: node_id,
            content: raw_step_node,
            step: step_detail.clone(),
            retries,
            summary: entry_summary,
        });
        step_details.push(step_detail);
    }

    let last_step = spec.steps.last().expect("至少存在一个 Step");
    let turn_summary =
        hand_turn_summary(&last_step.progress, &last_step.attempts, &last_step.slots);
    let usage = hand_usage_summary(&all_attempts);
    let tip_node_id = NodeId(user_node_id.0 + total_steps as u64);

    let turn_detail = TurnDetail {
        turn_id: spec.turn_id,
        session_id: spec.session_id,
        user_node: user_node_id,
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
            id: user_node_id,
            parent: Some(NodeId(0)),
            turn_id: Some(spec.turn_id),
            kind: NodeKind::UserMessage,
        },
        current: false,
        turn_start: Some(TurnLogSummary {
            turn_id: spec.turn_id,
            summary: turn_summary,
            agent_name: agent_coder(),
        }),
    });
    for ordinal in 0..total_steps {
        let id = NodeId(user_node_id.0 + 1 + ordinal as u64);
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
            turn_start: None,
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
            ordinal: spec.turn_ordinal,
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
    let binding = binding_for_call(&call, "builtin:read_file@v1", ARGS_CARGO_TOML_DIGEST_HEX);
    let slot = InvocationSlot {
        binding,
        progress: SlotProgress::Terminal {
            result: StoredToolResult {
                invocation_id: invocation_id_1(),
                name: "read_file".to_owned(),
                payload: ResultPayload::Succeeded(success_content_json_sample()),
            },
        },
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
        turn_ordinal: 1,
        effort: Some(Effort::High),
        user_prompt: "请查看 Cargo.toml 的行数",
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
        turn_ordinal: 1,
        effort: Some(Effort::Medium),
        user_prompt: "检查服务连通性",
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

/// 场景 3：工具失败（原始 `InvocationSlot` 与投影 `SlotDetail` 同为 `Failed(not_found)`，模型在下一步说明原因后结束）。
pub(super) fn scenario_tool_failed() -> BuiltScenario {
    let call = bound_call_read_missing(turn_id_3(), 0, 0, invocation_id_1());
    let binding = binding_for_call(&call, "builtin:read_file@v1", ARGS_MISSING_TXT_DIGEST_HEX);
    let failed_slot = InvocationSlot {
        binding,
        progress: SlotProgress::Terminal {
            result: StoredToolResult {
                invocation_id: invocation_id_1(),
                name: "read_file".to_owned(),
                payload: ResultPayload::Failed(tool_error_not_found()),
            },
        },
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
        turn_ordinal: 1,
        effort: Some(Effort::Low),
        user_prompt: "读取 missing.txt",
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
    let slot_0 = InvocationSlot {
        binding: binding_for_call(&call_0, "builtin:run_shell@v1", ARGS_SLEEP_CMD_DIGEST_HEX),
        progress: SlotProgress::Terminal {
            result: StoredToolResult {
                invocation_id: invocation_id_1(),
                name: "run_shell".to_owned(),
                payload: ResultPayload::Indeterminate(IndeterminateCause::Cancelled),
            },
        },
    };
    let slot_1 = InvocationSlot {
        binding: binding_for_call(&call_1, "builtin:read_file@v1", ARGS_CARGO_TOML_DIGEST_HEX),
        progress: SlotProgress::Registered,
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
        turn_ordinal: 1,
        effort: None,
        user_prompt: "运行耗时命令后读取配置",
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
        turn_ordinal: 1,
        effort: None,
        user_prompt: "刚接受尚未发出请求的轮次",
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
        turn_ordinal: 1,
        effort: Some(Effort::Medium),
        user_prompt: "模型流式响应中途进程退出",
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
    let slot_0 = InvocationSlot {
        binding: binding_for_call(&call_0, "builtin:read_file@v1", ARGS_CARGO_TOML_DIGEST_HEX),
        progress: SlotProgress::Terminal {
            result: StoredToolResult {
                invocation_id: invocation_id_1(),
                name: "read_file".to_owned(),
                payload: ResultPayload::Succeeded(success_content_json_sample()),
            },
        },
    };
    let slot_1 = InvocationSlot {
        binding: binding_for_call(&call_1, "builtin:read_file@v1", ARGS_MISSING_TXT_DIGEST_HEX),
        progress: SlotProgress::Registered,
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
        turn_ordinal: 1,
        effort: Some(Effort::High),
        user_prompt: "依次读取两个文件",
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
    let started_slot = InvocationSlot {
        binding: binding_for_call(&call, "builtin:run_shell@v1", ARGS_SLEEP_CMD_DIGEST_HEX),
        progress: SlotProgress::Started,
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
        turn_ordinal: 1,
        effort: Some(Effort::High),
        user_prompt: "执行外部脚本直至轮次超时",
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
            ("user_message", NodeKind::UserMessage => NodeKind::UserMessage),
            ("step", NodeKind::Step { .. } => NodeKind::Step { ordinal: 0 }),
        }
    );

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

    enum_fragments!(
        book,
        "attempt_progress",
        AttemptProgress {
            ("sent", AttemptProgress::Sent => AttemptProgress::Sent),
            (
                "ended",
                AttemptProgress::Ended { .. } => AttemptProgress::Ended {
                    end: AttemptEnd::Completed,
                    diagnostic: AttemptDiagnostic {
                        provider_error: None,
                        invalid_batch: None,
                    },
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
        "slot_progress",
        SlotProgress {
            ("registered", SlotProgress::Registered => SlotProgress::Registered),
            ("started", SlotProgress::Started => SlotProgress::Started),
            (
                "terminal",
                SlotProgress::Terminal { .. } => SlotProgress::Terminal {
                    result: StoredToolResult {
                        invocation_id: invocation_id_1(),
                        name: "read_file".to_owned(),
                        payload: ResultPayload::Succeeded(success_content_json_sample()),
                    },
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

    let ConversationEntry::UserMessage {
        content: ref raw_user_node,
        ..
    } = success.entries[0]
    else {
        unreachable!()
    };
    let ConversationEntry::Step {
        content: ref raw_continue_step_node,
        ..
    } = success.entries[1]
    else {
        unreachable!()
    };

    enum_fragments!(
        book,
        "node",
        Node {
            ("root", Node::Root => Node::Root),
            ("user_message", Node::UserMessage { .. } => raw_user_node.clone()),
            ("step", Node::Step { .. } => raw_continue_step_node.clone()),
        }
    );

    // ConversationEntry::Step 片段直接取 Continue 步骤（summary 为 None，验证 skip_serializing_if 省略形态）。
    enum_fragments!(
        book,
        "conversation_entry",
        ConversationEntry {
            ("user_message", ConversationEntry::UserMessage { .. } => success.entries[0].clone()),
            ("step", ConversationEntry::Step { .. } => success.entries[1].clone()),
        }
    );

    let turn_summary_full = indeterminate.turn_detail.summary.clone();
    let turn_summary_minimal = success.turn_detail.summary.clone();
    book.insert_struct_pair("turn_summary", &turn_summary_full, &turn_summary_minimal);

    let attempt_diag_full = AttemptDiagnostic {
        provider_error: Some("invalid tool arguments".to_owned()),
        invalid_batch: Some(ToolCallValidation::MalformedArguments { call_ordinal: 0 }),
    };
    let attempt_diag_minimal = AttemptDiagnostic {
        provider_error: None,
        invalid_batch: None,
    };
    book.insert_struct_pair(
        "attempt_diagnostic",
        &attempt_diag_full,
        &attempt_diag_minimal,
    );

    book.insert_struct_pair(
        "attempt_record",
        &retryable_failed_attempt_full(),
        &sent_attempt_minimal(),
    );

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

    let node_header_full = success.session_log.nodes[1].node.clone();
    let node_header_minimal = success.session_log.nodes[0].node.clone();
    book.insert_struct_pair("node_header", &node_header_full, &node_header_minimal);

    let log_row_full = success.session_log.nodes[1].clone();
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
