// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 状态消息与退出报告的显示文案；状态消息的等级也只在这里决定。
//! 后端给出等级的消息沿用后端的等级，TUI 自己产生的按消息种类定级。
//! 不调度请求、订阅或退出交接。

use crate::app::Status;
use lattice_api::local::{
    CancelStatus, ConnectionStatus, DetachReport, LocalError, ModelAttemptKey, Notice, NoticeLevel,
    PublicTurnOutcome, SubmissionResult, TurnId, TurnSummary,
};

fn status(level: NoticeLevel, text: String) -> Status {
    Status {
        level,
        text,
        closing: None,
        retry: None,
    }
}

fn recovered_notice(turn_id: TurnId) -> String {
    format!("按 TurnId 查到已接受的请求：turn {turn_id}；未重复执行")
}

fn not_accepted_notice(turn_id: TurnId) -> String {
    format!("请求 {turn_id} 未被接受：按 TurnId 查询确认后台没有这个请求，输入已保留")
}

fn not_submitted_notice(turn_id: TurnId) -> String {
    format!("提交在发出前取消：turn {turn_id}；这次提交没有发出")
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

/// 接受结果不明后按 TurnId 查到这一轮已被接受。直接收到的接受回执由助手标题线表达，不推入。
pub(crate) fn recovered_status(turn_id: TurnId) -> Status {
    status(NoticeLevel::Info, recovered_notice(turn_id))
}

pub(crate) fn not_accepted_status(turn_id: TurnId) -> Status {
    status(NoticeLevel::Error, not_accepted_notice(turn_id))
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

pub(crate) fn older_turns_unavailable_status(error: &LocalError) -> Status {
    status(
        NoticeLevel::Warn,
        format!(
            "更早的轮读取失败；再次翻到顶部时重新读取：{}",
            error.message
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

/// 会话头中后台已知的这一轮执行诊断。
pub(crate) fn execution_diagnostic_status(error: &LocalError) -> Status {
    status(NoticeLevel::Error, format!("执行诊断：{}", error.message))
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

/// 重试提示沿用 Step 条目中该提示的等级。
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
        report.session_id,
        report
            .turn_id
            .map_or_else(|| "未知".into(), |id| id.to_string())
    );
    if let Some(receipt) = &report.submission {
        lines.push(match &receipt.result {
            SubmissionResult::Accepted { .. } if receipt.recovered => {
                let notice = recovered_notice(receipt.turn_id);
                format!("请求 {identity}：{notice}")
            }
            SubmissionResult::Accepted { .. } => format!("请求 {identity}：已确认接受"),
            SubmissionResult::Rejected { error } => {
                let message = &error.message;
                format!("请求 {identity} 被拒绝或回执失败：{message}")
            }
            SubmissionResult::NotAccepted => not_accepted_notice(receipt.turn_id),
            SubmissionResult::Unknown { error } => {
                let detail = error
                    .as_ref()
                    .map_or_else(String::new, |error| format!("：{}", error.message));
                format!("提交结果未知：{identity}；请按原身份查询{detail}")
            }
        });
    }
    if let Some(status) = report.cancel {
        match status {
            CancelStatus::Taken | CancelStatus::Moot => {
                let message = cancel_message(status);
                lines.push(format!("{message}：{identity}"))
            }
            CancelStatus::Pending | CancelStatus::Unknown | CancelStatus::NotTaken => {
                let message = cancel_message(status);
                lines.push(format!(
                    "取消未确认：{identity}；{message}；后台可能仍接受或执行，请按原身份查询"
                ))
            }
        }
    }
    if let Some(error) = report.cancel_error {
        let message = error.message;
        lines.push(format!("取消诊断：{identity}：{message}"));
    }
    if let Some(error) = report.submission_diagnostic {
        let message = error.message;
        lines.push(format!(
            "提交结果未知：{identity}；{message}；已知观察与完成结果保持"
        ));
    }
    // 未关闭且仍有 Owner 时 summary 只说明此刻停下会得到什么，不是结局。
    let ended = report.summary.as_ref().is_some_and(|summary| {
        summary.progress.is_closed()
            || report
                .head
                .as_ref()
                .is_some_and(|head| head.turn_id == report.turn_id && !head.owner)
    });
    if ended
        && !closing_shown
        && let (Some(turn_id), Some(summary)) = (report.turn_id, &report.summary)
    {
        let word = outcome_word(&summary.outcome);
        lines.push(match &summary.notice {
            Some(notice) => {
                let text = &notice.text;
                format!("Turn {turn_id} {word}：{text}")
            }
            None => format!("Turn {turn_id} {word}"),
        });
    }
    lines
}
