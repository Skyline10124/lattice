// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use futures::stream::Stream;
use lattice_profile::LoadedAgentProfile;
use lattice_session::pending::{ClosureOrigin, TurnOutcome, WorkspaceHistoryCheckpoint};

/// 各执行入口共用原有请求形状；显式请求身份和连接材料由 [`crate::start_turn`] 单独接收。
///
/// [`crate::run_turn`] 等待持久完成， [`crate::stream_turn`] 在接受或恢复回执后返回观察流。
/// 恢复已有 pending 时只报告旧 Turn，不接受此处的新输入。
#[derive(Clone, Serialize, Deserialize)]
pub struct RunTurnRequest {
    pub input: String,
    pub session_id: String,
    #[serde(skip)]
    pub loaded_profile: Option<LoadedAgentProfile>,
    /// Cooperative cancellation flag forwarded to the agent loop.
    #[serde(skip)]
    pub cancel_flag: Option<Arc<AtomicBool>>,
    /// Test-only LLM injection for lattice-api integration tests.
    #[serde(skip)]
    pub test_llm: Option<Arc<dyn lattice_agent::LlmClient>>,
    /// Test-only project root for CEP runtime wiring.
    #[serde(skip)]
    pub test_project_root: Option<String>,
}

impl PartialEq for RunTurnRequest {
    fn eq(&self, other: &Self) -> bool {
        self.input == other.input
            && self.session_id == other.session_id
            && self.loaded_profile == other.loaded_profile
    }
}

/// A stage in the turn execution pipeline.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum RunTurnStage {
    Profile,
    Session,
    Agent,
    Llm,
    Tools,
    Commit,
    Render,
}

impl std::fmt::Display for RunTurnStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunTurnStage::Profile => write!(f, "profile"),
            RunTurnStage::Session => write!(f, "session"),
            RunTurnStage::Agent => write!(f, "agent"),
            RunTurnStage::Llm => write!(f, "llm"),
            RunTurnStage::Tools => write!(f, "tools"),
            RunTurnStage::Commit => write!(f, "commit"),
            RunTurnStage::Render => write!(f, "render"),
        }
    }
}

/// Summary of a single tool execution.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolResultSummary {
    pub tool_call_id: String,
    pub name: String,
    pub ok: bool,
    pub output_len: usize,
    pub error_summary: Option<String>,
}

/// Token usage statistics.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TurnUsageSummary {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

impl From<lattice_session::TurnUsage> for TurnUsageSummary {
    fn from(u: lattice_session::TurnUsage) -> Self {
        Self {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
        }
    }
}

/// Result of executing a synchronous turn.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RunTurnResult {
    pub session_id: String,
    pub commit_id: String,
    pub assistant_output: String,
    pub outcome: TurnOutcome,
    pub origin: ClosureOrigin,
    pub workspace_history_checkpoint: WorkspaceHistoryCheckpoint,
    pub usage: Option<TurnUsageSummary>,
    pub tool_results: Vec<ToolResultSummary>,
    pub stages: Vec<RunTurnStage>,
}

/// Public turn stream event with stable JSON discriminator.
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
    /// The turn has started execution.
    TurnStarted {
        session_id: String,
        /// Additive stable turn identifier (`<session>:<parent-or-root>`).
        /// Audit replay needs it to address a turn deterministically; old
        /// consumers ignore the extra field.
        turn_id: String,
        parent_commit_id: Option<String>,
        branch: Option<String>,
    },
    /// A model request is starting.
    ModelStarted {
        provider: Option<String>,
        model: String,
    },
    /// A model request failed before a stream opened.
    LlmRequestError {
        provider: Option<String>,
        model: String,
        attempt: u32,
        max_attempts: u32,
        error_kind: String,
        retryable: bool,
        will_retry: bool,
        delay_ms: Option<u64>,
        status_code: Option<u16>,
        message: String,
    },
    /// A chunk of reasoning/thinking text from the LLM.
    ReasoningDelta { content: String },
    /// A chunk of visible assistant text from the LLM.
    TokenDelta { content: String },
    /// A tool call is about to execute.
    ToolCallStarted { id: String, name: String },
    /// A tool call has completed.
    ToolCallFinished {
        id: String,
        name: String,
        ok: bool,
        output_len: usize,
        error_summary: Option<String>,
    },
    /// A model request completed successfully.
    ModelFinished {
        finish_reason: String,
        usage: Option<TurnUsageSummary>,
    },
    /// The turn failed due to an error.
    TurnFailed { reason: String },
    /// The turn was cancelled.
    TurnCancelled { reason: Option<String> },
    /// Commit persistence is in progress.
    TurnCommitting,
    /// The turn was committed successfully.
    TurnCommitted {
        session_id: String,
        commit_id: String,
        outcome: TurnOutcome,
        origin: ClosureOrigin,
        workspace_history_checkpoint: WorkspaceHistoryCheckpoint,
    },
    /// Terminal/evidence publication exhausted its fixed durability budget.
    EvidenceDurabilityFailed { attempts: u32 },
    /// Durable invocation facts require a closure-only reconciliation pass.
    RecoveryRequired {
        reason: crate::session::RecoveryRequired,
    },
    /// Commit persistence failed.
    CommitFailed { reason: String },
    /// A CEP call has started on the embedded server.
    CepCallStarted {
        invocation_id: lancet_protocol::InvocationId,
        method: String,
        semantic_stage: String,
        guard_stage: String,
    },
    /// A CEP call has finished on the embedded server.
    CepCallFinished {
        invocation_id: lancet_protocol::InvocationId,
        method: String,
        ok: bool,
        semantic_stage: String,
        guard_stage: String,
        error_code: Option<String>,
    },
}

/// 接受确认来自持久发布返回值，与有损事件通道无关。
#[derive(Clone, Debug, PartialEq)]
pub enum TurnAcceptance {
    /// 仅在 TurnStarted 已确认持久后返回。
    Accepted { turn_id: lancet_protocol::TurnId },
    /// 找回同一请求；Pending 只报告日志，不在此恢复或重新执行。
    Existing(Box<lattice_session::StoredTurn>),
    /// 关闭的是既有 Turn，本次新 prompt 没有被接受。
    Recovered(Box<lattice_session::TurnCommit>),
    /// 发布结果不确定；不能据此重分配身份，必须查询或重试同一 TurnId。
    ConfirmationUnknown { turn_id: lancet_protocol::TurnId },
}

#[derive(Clone, Debug, PartialEq)]
pub enum TurnCompletion {
    Committed(Box<lattice_session::TurnCommit>),
    /// 本次所有者任务已结束，但持久 Turn 仍待独立恢复；不是提交完成。
    Pending(lancet_protocol::TurnId),
}

/// 释放观察者或回执不取消任务；后台排空必须等待 completion，而非事件 EOF。
pub struct TurnExecution {
    pub acceptance: tokio::sync::oneshot::Receiver<crate::session::Result<TurnAcceptance>>,
    pub completion: tokio::sync::oneshot::Receiver<crate::session::Result<TurnCompletion>>,
    pub events: SequencedTurnEventStream,
    /// 包括未投递事件的最终序号，用于识别尾部缺口；不是持久状态。
    pub event_sequence: Arc<std::sync::atomic::AtomicU64>,
    pub cancel_flag: Arc<AtomicBool>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SequencedTurnEvent {
    pub sequence: u64,
    pub event: TurnEvent,
}

pub type SequencedTurnEventStream = Pin<Box<dyn Stream<Item = SequencedTurnEvent> + Send>>;

/// Boxed stream of TurnEvent values.
pub type TurnEventStream = Pin<Box<dyn Stream<Item = TurnEvent> + Send>>;
