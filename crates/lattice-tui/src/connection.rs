// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 状态消息与退出报告的显示文案；状态消息的等级也只在这里决定。
//! 后端给出等级的消息沿用后端的等级，TUI 自己产生的按消息种类定级。
//! 不调度请求、订阅或退出交接。

use crate::app::Status;
use lattice_api::local::{
    Acceptance, CancelStatus, ConnectionStatus, DetachReport, HistoryReadContext, HistoryReadError,
    LocalError, ModelAttemptKey, Notice, NoticeLevel, PublicTurnOutcome, TurnId, TurnSummary,
};

fn status(level: NoticeLevel, text: String) -> Status {
    Status {
        level,
        text,
        closing: None,
        retry: None,
    }
}

fn submission_notice(acceptance: &Acceptance) -> String {
    match acceptance {
        Acceptance::Accepted { turn } => format!(
            "已确认接受：session {} turn {}",
            turn.session_id, turn.turn_id
        ),
        Acceptance::Existing { turn } => format!(
            "找回既有请求：session {} turn {}；未重复执行",
            turn.session_id, turn.turn_id
        ),
        Acceptance::ConfirmationUnknown { turn_id, .. } => {
            format!("接受确认未知：{turn_id}；保留此 ID 查询，不分配新请求")
        }
        Acceptance::NeedsConfiguration { .. } => "本地响应异常".into(),
    }
}

fn not_submitted_notice(turn_id: TurnId) -> String {
    format!("本客户端已停止后续配置提交：turn {turn_id}；不表示该身份全局未执行")
}

fn cancel_message(status: CancelStatus) -> &'static str {
    match status {
        CancelStatus::Pending => "已请求取消；等待后台接管回执",
        CancelStatus::Taken => "后台已接管取消；持久终态仍待确认",
        CancelStatus::NotTaken => "后台未接管本次取消；不表示没有后台工作，请查询原 Turn",
        CancelStatus::Unknown => "取消确认未知；请按 TurnId 查询或再次明确取消",
        CancelStatus::Moot => "本次取消交接已结束；不作用于其他 Turn，已知结果保持",
    }
}

fn outcome_word(outcome: &PublicTurnOutcome) -> &'static str {
    match outcome {
        PublicTurnOutcome::Completed => "完成",
        PublicTurnOutcome::Failed => "失败",
        PublicTurnOutcome::Cancelled => "已取消",
    }
}

fn history_context(context: &HistoryReadContext) -> String {
    match context {
        HistoryReadContext::Worker => "读取任务".into(),
        HistoryReadContext::Page {
            cursor: Some(cursor),
            ..
        } => format!("分页（自节点 {} 起）", cursor.node.0),
        HistoryReadContext::Page { cursor: None, .. } => "分页（首页）".into(),
        HistoryReadContext::Node { node } => format!("节点 {}", node.node.0),
    }
}

/// 接受回执。Accepted 由助手标题线表达，调用方不推入。
pub(crate) fn submission_status(acceptance: &Acceptance) -> Status {
    let level = match acceptance {
        Acceptance::Accepted { .. } | Acceptance::Existing { .. } => NoticeLevel::Info,
        Acceptance::ConfirmationUnknown { .. } => NoticeLevel::Warn,
        Acceptance::NeedsConfiguration { .. } => NoticeLevel::Error,
    };
    status(level, submission_notice(acceptance))
}

pub(crate) fn rejected_status(turn_id: TurnId, error: &LocalError) -> Status {
    status(
        NoticeLevel::Error,
        format!("请求 {} 被拒绝或回执失败：{}", turn_id, error.message),
    )
}

pub(crate) fn submission_unknown_status(turn_id: TurnId, error: Option<&LocalError>) -> Status {
    let mut text = format!("提交结果未知：{turn_id}；继续观察同一请求，不自动重发");
    if let Some(error) = error {
        text.push('\n');
        text.push_str(&error.message);
    }
    status(NoticeLevel::Warn, text)
}

pub(crate) fn not_submitted_status(turn_id: TurnId) -> Status {
    status(NoticeLevel::Info, not_submitted_notice(turn_id))
}

/// 取消状态与它的诊断是同一件事：合成一条消息，带诊断时按 Error。
pub(crate) fn cancellation_status(
    turn_id: TurnId,
    cancel: CancelStatus,
    error: Option<&LocalError>,
) -> Status {
    let mut text = format!("{}：{}", cancel_message(cancel), turn_id);
    let level = match (error, cancel) {
        (Some(error), _) => {
            text.push_str("\n取消诊断：");
            text.push_str(&error.message);
            NoticeLevel::Error
        }
        (None, CancelStatus::NotTaken | CancelStatus::Unknown) => NoticeLevel::Warn,
        (None, CancelStatus::Pending | CancelStatus::Taken | CancelStatus::Moot) => {
            NoticeLevel::Info
        }
    };
    status(level, text)
}

pub(crate) fn history_unavailable_status(stale: bool, error: &HistoryReadError) -> Status {
    status(
        NoticeLevel::Warn,
        format!(
            "{}持久正文不可用；此次内容未验证，/reconnect 重载：{}：{}",
            if stale { "旧切点" } else { "" },
            history_context(&error.context),
            error.error.message
        ),
    )
}

pub(crate) fn observation_lost_status(cause: &ConnectionStatus) -> Status {
    let text = match cause {
        ConnectionStatus::Eof => "观察连接结束；不代表 Turn 已关闭，/reconnect 重连".into(),
        ConnectionStatus::Idle => format!(
            "观察 {} s 无增量；执行状态未知，/reconnect 重连",
            lattice_api::STREAM_IDLE_TIMEOUT_SECS
        ),
        ConnectionStatus::Error(error) => format!(
            "观察连接出错；执行状态未知，/reconnect 重连：{}",
            error.message
        ),
    };
    status(NoticeLevel::Warn, text)
}

pub(crate) fn observation_diagnostic_status(error: &LocalError) -> Status {
    status(NoticeLevel::Error, format!("观察诊断：{}", error.message))
}

pub(crate) fn orphaned_turn_status(turn_id: TurnId) -> Status {
    status(
        NoticeLevel::Warn,
        format!("turn {turn_id} 的执行者已经消失，它停在已写入的进度；可以直接发送新消息或退回"),
    )
}

pub(crate) fn discarded_comments_status(numbers: &[usize]) -> Status {
    status(
        NoticeLevel::Warn,
        format!(
            "锚点 Step 不在当前历史，丢弃评论：{}",
            numbers
                .iter()
                .map(|n| format!("#{n}"))
                .collect::<Vec<_>>()
                .join("、")
        ),
    )
}

/// 旧句柄退出报告中的一行；只作历史说明，不代表当前目标的状态。
pub(crate) fn handoff_status(line: &str) -> Status {
    status(NoticeLevel::Info, format!("历史交接：{line}"))
}

/// 重试提示沿用后端给出的等级；实时 `LlmRetry` 与历史带回的提示共用这一格式。
/// `key` 是失败的那次尝试，`attempt_ordinal` 从 0 起。
pub(crate) fn retry_status(key: ModelAttemptKey, delay_ms: u64, notice: Notice) -> Status {
    Status {
        level: notice.level,
        text: format!(
            "第 {} 次请求失败，{} ms 后重试：{}",
            key.attempt_ordinal.saturating_add(1),
            delay_ms,
            notice.text
        ),
        closing: None,
        retry: Some(key),
    }
}

/// Turn 结局沿用 notice 的等级；Completed 没有 notice，按 Info。结局符号由渲染层添加。
pub(crate) fn closing_status(turn_id: TurnId, summary: TurnSummary) -> Status {
    let word = outcome_word(&summary.outcome);
    let (level, text) = match summary.notice {
        Some(notice) => (
            notice.level,
            format!("turn {} {}：{}", turn_id, word, notice.text),
        ),
        None => (NoticeLevel::Info, format!("turn {turn_id} {word}")),
    };
    Status {
        level,
        text,
        closing: Some((turn_id, summary.outcome)),
        retry: None,
    }
}

/// 仅格式化共同句柄交付的退出报告；五秒交接及其判定全部属于 LocalTurn。
/// `closing_shown` 表示这个 Turn 的结局已显示在对话记录中，不再重述。
pub(crate) fn detach_lines(report: DetachReport, closing_shown: bool) -> Vec<String> {
    let mut lines = Vec::new();
    let identity = format!(
        "session {} turn {}",
        crate::app::session_label(report.session_id),
        report
            .turn_id
            .map_or_else(|| "未知".into(), |id| id.to_string())
    );
    if let Some(result) = &report.submission {
        lines.push(match result {
            Ok(acceptance) => format!("请求 {identity}：{}", submission_notice(acceptance)),
            Err(error) => format!("请求 {identity} 被拒绝或回执失败：{}", error.message),
        });
    }
    if let (Some(id), Some(_)) = (report.turn_id, report.not_submitted) {
        lines.push(not_submitted_notice(id));
    }
    if let Some(status) = report.cancel {
        match status {
            CancelStatus::Taken | CancelStatus::Moot => {
                lines.push(format!("{}：{identity}", cancel_message(status)))
            }
            CancelStatus::Pending | CancelStatus::Unknown | CancelStatus::NotTaken => {
                lines.push(format!(
                    "取消未确认：{identity}；{}；后台可能仍接受或执行，请按原身份查询",
                    cancel_message(status)
                ))
            }
        }
    }
    if let Some(error) = report.cancel_error {
        lines.push(format!("取消诊断：{}：{}", identity, error.message));
    }
    if let Some(error) = report.submission_diagnostic {
        lines.push(format!(
            "提交结果未知：{identity}；{}；已知观察与完成结果保持",
            error.message
        ));
    }
    if let Some(state) = report.state {
        // 未关闭且仍有 Owner 时 summary 只说明此刻停下会得到什么，不是结局。
        if let Some(turn) = &state.view.selected_turn
            && (turn.summary.progress.is_closed() || !state.view.owned)
            && !closing_shown
        {
            let summary = &turn.summary;
            let word = outcome_word(&summary.outcome);
            lines.push(match &summary.notice {
                Some(notice) => format!("Turn {} {}：{}", turn.turn_id, word, notice.text),
                None => format!("Turn {} {}", turn.turn_id, word),
            });
        }
        if let Some(error) = state.error {
            let target = state
                .view
                .selected_turn
                .as_ref()
                .map(|turn| turn.turn_id)
                .or(report.turn_id);
            lines.push(format!(
                "观察诊断：session {} turn {}：{}",
                crate::app::session_label(report.session_id),
                target.map_or_else(|| "未知".into(), |id| id.to_string()),
                error.message
            ));
        }
    }
    if let Some(error) = report.history_error {
        lines.push(format!(
            "持久正文不可用：{}，{}：{}",
            identity,
            history_context(&error.context),
            error.error.message
        ));
    }
    lines
}
