// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Provider-neutral 工具结果闭合枚举。
//!
//! 执行结果是封闭枚举。每个 `ToolCall` 都由且仅由其中一种结果闭合，事后不再改标。
//! `lattice-session` 的持久调用槽位保存同一套结果词汇以及按结果形态区分的载荷
//! （见 `docs/architecture/session/steps.md#调用槽位`）。
//!
//! 与模型调用尝试一样分为两个维度：结果表示终态形态，[`crate::IndeterminateCause`] 或执行器返回的
//! [`crate::ToolError`] 承载具体原因。只有 [`ToolResultOutcome::NotExecuted`] 表示调用从未交给执行器，
//! 保证无外部效果；其余结果都在槽位越过 `Started` 之后记录。派发后的取消记为
//! [`ToolResultOutcome::Indeterminate`]（`Cancelled`）。

use serde::{Deserialize, Serialize};

/// 工具执行结果的封闭枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolResultOutcome {
    /// 执行器返回 JSON 结果。
    Succeeded,
    /// 执行器返回 `ToolError`。
    Failed,
    /// 从未交给执行器；停止原因记在 Step 的结束原因中，是唯一保证无外部效果的结果。
    NotExecuted,
    /// 槽位越过 `Started` 但没有确定结果，外部效果未知。
    Indeterminate,
}

impl ToolResultOutcome {
    /// 稳定的编码与摘要拼写。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::NotExecuted => "not_executed",
            Self::Indeterminate => "indeterminate",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_wire_spellings_are_stable() {
        let cases = [
            (ToolResultOutcome::Succeeded, "succeeded"),
            (ToolResultOutcome::Failed, "failed"),
            (ToolResultOutcome::NotExecuted, "not_executed"),
            (ToolResultOutcome::Indeterminate, "indeterminate"),
        ];
        for (outcome, spelling) in cases {
            assert_eq!(outcome.as_str(), spelling);
            let json = serde_json::to_string(&outcome).expect("serialize");
            assert_eq!(json, format!("\"{spelling}\""));
            let back: ToolResultOutcome = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(outcome, back);
        }
    }
}
