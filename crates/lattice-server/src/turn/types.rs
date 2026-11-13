// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use lattice_conversation::SessionId;
use lattice_session::store::{SettingsVersion, TurnDetail};
#[cfg(test)]
use lattice_session::store::{TurnSummary, UsageSummary};

/// 执行核心的冻结输入；会话设置由会话库持有，请求配置与凭据由后台网关读取。
#[derive(Clone)]
pub struct RunTurnRequest {
    pub input: String,
    pub session_id: SessionId,
    pub expected_settings: Option<SettingsVersion>,
    pub cancel_flag: Option<Arc<AtomicBool>>,
    /// 注入本轮工具执行器；未指定时使用默认生产实现（或 `test-fake-tools` 环境变量选择）。
    pub tools: Option<Arc<dyn super::tools::ToolExecutor>>,
    /// 注入客户端、协议及最终模型的可用档位，由测试提供能力来源。
    #[cfg(any(test, feature = "test-utils"))]
    pub test_llm: Option<(
        Arc<dyn lattice_agent::LlmClient>,
        lattice_session::store::AttemptProtocol,
        Vec<lattice_conversation::Effort>,
    )>,
}

impl RunTurnRequest {
    pub(crate) fn injected_llm(
        &self,
    ) -> Option<(
        Arc<dyn lattice_agent::LlmClient>,
        lattice_session::store::AttemptProtocol,
        Vec<lattice_conversation::Effort>,
    )> {
        #[cfg(any(test, feature = "test-utils"))]
        return self.test_llm.clone();
        #[cfg(not(any(test, feature = "test-utils")))]
        None
    }
}

#[cfg(test)]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct RunTurnResult {
    pub turn: TurnRef,
    pub summary: TurnSummary,
    pub usage: UsageSummary,
    pub steps: Vec<lattice_session::store::StepDetail>,
}

/// 所有终结事件仅来自已经提交的关闭事实。
///
/// 按 `#[serde(tag = "type", rename_all = "snake_case")]` 序列化为 `{"type": "...", ...}`。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum TurnEvent {
    TurnStarted {
        turn: TurnRef,
        user_node: lattice_session::store::NodeId,
    },
    ModelStarted {
        key: lattice_conversation::ModelAttemptKey,
        origin: lattice_session::store::AttemptOrigin,
        model: String,
    },
    TokenDelta {
        key: lattice_conversation::ModelAttemptKey,
        content: String,
    },
    ReasoningDelta {
        key: lattice_conversation::ModelAttemptKey,
        content: String,
    },
    LlmRetry {
        key: lattice_conversation::ModelAttemptKey,
        delay_ms: u64,
        notice: lattice_session::store::Notice,
    },
    ModelFinished {
        key: lattice_conversation::ModelAttemptKey,
        finish_reason: Option<String>,
        usage: Option<lattice_conversation::TokenUsage>,
        usage_kind: lattice_conversation::UsageCompleteness,
        cost: Option<lattice_conversation::CostEstimate>,
    },
    CallsRegistered {
        key: lattice_session::store::StepKey,
        calls: Vec<lattice_session::store::InvocationBinding>,
    },
    ToolCallStarted {
        id: lattice_conversation::InvocationId,
        name: String,
    },
    ToolCallFinished {
        id: lattice_conversation::InvocationId,
        name: String,
        outcome: lattice_conversation::ToolResultOutcome,
        material: lattice_session::store::MaterialSummary,
        error_summary: Option<String>,
    },
    StepClosed {
        step: lattice_session::store::StepDetail,
    },
    TurnCompleted {
        turn: TurnRef,
        step: lattice_session::store::StepDetail,
        summary: lattice_session::store::TurnSummary,
    },
    TurnFailed {
        turn: TurnRef,
        step: lattice_session::store::StepDetail,
        summary: lattice_session::store::TurnSummary,
    },
    TurnCancelled {
        turn: TurnRef,
        step: lattice_session::store::StepDetail,
        summary: lattice_session::store::TurnSummary,
    },
}

pub(crate) use lattice_ipc::{TextSegment, TurnRef};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct AttemptSegments {
    pub key: lattice_conversation::ModelAttemptKey,
    pub segments: Vec<TextSegment>,
}
/// 接受回执与独立完成回执都不从观察窗口推导。
#[derive(Clone, Debug, PartialEq)]
pub enum TurnAcceptance {
    Accepted {
        turn: TurnDetail,
    },
    ConfirmationUnknown {
        turn_id: lattice_conversation::TurnId,
        session_id: SessionId,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum TurnCompletion {
    Closed(Box<TurnDetail>),
}

pub struct TurnExecution {
    pub acceptance: tokio::sync::oneshot::Receiver<
        std::result::Result<TurnAcceptance, crate::session::ApiError>,
    >,
    pub completion: tokio::sync::oneshot::Receiver<
        std::result::Result<TurnCompletion, crate::session::ApiError>,
    >,
    #[cfg(any(test, feature = "test-utils"))]
    pub cancel_flag: Arc<AtomicBool>,
}
