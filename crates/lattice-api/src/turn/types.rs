// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use lancet_protocol::SessionId;
use lattice_profile::LoadedAgentProfile;
use lattice_session::store::{TurnDetail, TurnSummary, UsageSummary};

/// 执行核心的冻结输入；请求配置与凭据由后台网关读取。
#[derive(Clone)]
pub struct RunTurnRequest {
    pub input: String,
    pub session_id: Option<SessionId>,
    pub agent: Option<String>,
    pub loaded_profile: Option<LoadedAgentProfile>,
    pub cancel_flag: Option<Arc<AtomicBool>>,
    #[cfg(any(test, feature = "test-utils"))]
    pub test_llm: Option<(
        Arc<dyn lattice_agent::LlmClient>,
        lattice_session::store::AttemptProtocol,
    )>,
    #[cfg(any(test, feature = "test-utils"))]
    pub test_project_root: Option<String>,
}

impl RunTurnRequest {
    pub(crate) fn injected_llm(
        &self,
    ) -> Option<(
        Arc<dyn lattice_agent::LlmClient>,
        lattice_session::store::AttemptProtocol,
    )> {
        #[cfg(any(test, feature = "test-utils"))]
        return self.test_llm.clone();
        #[cfg(not(any(test, feature = "test-utils")))]
        None
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RunTurnResult {
    pub turn: TurnRef,
    pub summary: TurnSummary,
    pub usage: UsageSummary,
    pub steps: Vec<lattice_session::store::StepDetail>,
}

/// 所有终结事件仅来自已经提交的关闭事实。
///
/// Serializes with `#[serde(tag = "type", rename_all = "snake_case")]`, so
/// each variant produces JSON of the form `{"type": "snake_case_name", ...}`.
/// Unit variants serialize as `{"type": "snake_case_name"}`.
///
/// The vocabulary covers turn lifecycle (`TurnStarted`, terminal events,
/// commit events), per-request model activity (`ModelStarted`,
/// `ModelFinished`), incremental deltas (`TokenDelta`, `ReasoningDelta`),
/// and tool execution (`ToolCallStarted`, `ToolCallFinished`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TurnEvent {
    TurnStarted {
        turn: TurnRef,
        user_node: lattice_session::store::NodeId,
    },
    ModelStarted {
        key: lancet_protocol::ModelAttemptKey,
        origin: lattice_session::store::AttemptOrigin,
        model: String,
    },
    TokenDelta {
        key: lancet_protocol::ModelAttemptKey,
        content: String,
    },
    ReasoningDelta {
        key: lancet_protocol::ModelAttemptKey,
        content: String,
    },
    LlmRetry {
        key: lancet_protocol::ModelAttemptKey,
        delay_ms: u64,
        notice: lattice_session::store::Notice,
    },
    ModelFinished {
        key: lancet_protocol::ModelAttemptKey,
        finish_reason: Option<String>,
        usage: Option<lattice_conversation::TokenUsage>,
        usage_kind: lattice_conversation::UsageCompleteness,
        cost: Option<lattice_conversation::CostEstimate>,
    },
    CallsRegistered {
        key: lattice_session::store::StepKey,
        calls: Vec<lancet_protocol::InvocationBinding>,
    },
    ToolCallStarted {
        id: lancet_protocol::InvocationId,
        name: String,
    },
    ToolCallFinished {
        id: lancet_protocol::InvocationId,
        name: String,
        outcome: lattice_conversation::ToolResultOutcome,
        material: lattice_session::store::MaterialSummary,
        error_summary: Option<String>,
    },
    CepCallStarted {
        invocation_id: lancet_protocol::InvocationId,
        method: String,
        semantic_stage: String,
        guard_stage: String,
    },
    CepCallFinished {
        invocation_id: lancet_protocol::InvocationId,
        method: String,
        ok: bool,
        semantic_stage: String,
        guard_stage: String,
        error_code: Option<String>,
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

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TurnRef {
    pub session_id: lancet_protocol::SessionId,
    pub turn_id: lancet_protocol::TurnId,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttemptSegments {
    pub key: lancet_protocol::ModelAttemptKey,
    pub segments: Vec<TextSegment>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TextSegment {
    Text { text: String },
    Reasoning { text: String },
}
/// 接受回执与独立完成回执都不从观察窗口推导。
#[derive(Clone, Debug, PartialEq)]
pub enum TurnAcceptance {
    Accepted {
        turn: TurnDetail,
    },
    ConfirmationUnknown {
        turn_id: lancet_protocol::TurnId,
        session_id: SessionId,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum TurnCompletion {
    Closed(Box<TurnDetail>),
}

pub struct TurnExecution {
    pub acceptance: tokio::sync::oneshot::Receiver<crate::session::Result<TurnAcceptance>>,
    pub completion: tokio::sync::oneshot::Receiver<crate::session::Result<TurnCompletion>>,
    pub cancel_flag: Arc<AtomicBool>,
}
