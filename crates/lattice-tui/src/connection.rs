// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 本地客户端回执的显示文案；不调度请求、订阅或退出交接。

use lattice_api::local::{Acceptance, CancelStatus, DetachReport, TurnId, TurnObservation};

pub(crate) fn submission_notice(acceptance: &Acceptance) -> String {
    let (id, message) = match acceptance {
        Acceptance::Accepted { turn_id } => (*turn_id, "已确认接受"),
        Acceptance::ConfirmationUnknown { turn_id } => {
            (*turn_id, "接受确认未知；保留此 ID 查询，不分配新请求")
        }
        Acceptance::Existing {
            turn: TurnObservation::Pending(turn),
        } => (turn.turn_id, "找回既有 pending；未重复接受"),
        Acceptance::Existing {
            turn: TurnObservation::Committed(turn),
        } => (turn.turn_id, "找回既有结果；未重复执行"),
        Acceptance::Recovered { turn } => {
            (turn.turn_id, "旧 Turn 已恢复关闭；本次新 prompt 未接受")
        }
        Acceptance::NeedsConfiguration => return "本地响应异常".into(),
    };
    format!("{message}：{id}")
}

pub(crate) fn not_submitted_notice(turn_id: TurnId) -> String {
    format!("本客户端已停止后续配置提交：turn {turn_id}；不表示该身份全局未执行")
}

pub(crate) fn cancel_message(status: CancelStatus) -> &'static str {
    match status {
        CancelStatus::Pending => "已请求取消；等待后台接管回执",
        CancelStatus::Taken => "后台已接管取消；持久终态仍待确认",
        CancelStatus::NotTaken => "后台未接管本次取消；不表示没有后台工作，请查询或显式恢复",
        CancelStatus::Unknown => "取消确认未知；请按 TurnId 查询或再次明确取消",
        CancelStatus::Moot => "本次取消交接已结束；不作用于其他 Turn，已知结果保持",
    }
}

/// 仅格式化共同句柄交付的退出报告；五秒交接及其判定全部属于 LocalTurn。
pub(crate) fn detach_lines(report: DetachReport) -> Vec<String> {
    let mut lines = Vec::new();
    let identity = format!(
        "session {} turn {}",
        report.session_id,
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
        lines.push(format!(
            "取消诊断：{identity}，{:?}：{}",
            error.kind, error.message
        ));
    }
    if let Some(error) = report.submission_diagnostic {
        lines.push(format!(
            "提交结果未知：{identity}；{}；已知观察与完成结果保持",
            error.message
        ));
    }
    if let Some(state) = report.state {
        if let Some(TurnObservation::Committed(turn)) = &state.view.requested_turn {
            lines.push(format!(
                "Turn {}：{:?}，commit {}，完成确认 {:?}",
                turn.turn_id, turn.outcome, turn.commit_id, state.confirmation
            ));
        }
        if let Some(error) = state.error {
            let target = match &state.view.requested_turn {
                Some(TurnObservation::Committed(turn)) => Some(turn.turn_id),
                Some(TurnObservation::Pending(turn)) => Some(turn.turn_id),
                None => report.turn_id,
            };
            lines.push(format!(
                "观察诊断：session {} turn {}：{}",
                report.session_id,
                target.map_or_else(|| "未知".into(), |id| id.to_string()),
                error.message
            ));
        }
    }
    if let Some(error) = report.history_error {
        lines.push(format!(
            "持久正文不可用：{identity}，{:?}：{}",
            error.context, error.error.message
        ));
    }
    lines
}
