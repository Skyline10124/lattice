// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Provider-neutral, data-only conversation contract (ADR 0005 §7).
//!
//! This crate owns:
//!
//! - ordered [`NativeMessage`] / [`MessagePart`] values with the
//!   [`UnboundToolCall`] / [`BoundToolCall`] typestates;
//! - the provider-neutral Tool result closure ([`ToolResultOutcome`]);
//! - per-attempt usage and interruption/rejection records ([`attempt`]);
//! - provider origin and bounded part-local opaque replay state;
//! - 结构化 Step 与其调用结果，以及 provider-neutral 对话历史；
//! - 身份类型（[`SessionId`]、[`TurnId`]、[`InvocationId`]）、结构地址
//!   （[`ModelMessageAddress`]、[`ToolCallAddress`]、[`ModelAttemptKey`]）、
//!   规范 JSON（[`canonical_json`]）与参数摘要（[`arguments_digest`]）。
//!
//! 本 crate 仅依赖中立的数据序列化、摘要与错误库，不持有文件系统、存储、锁、
//! 运行时、provider HTTP 客户端或 ID 生成器。

mod address;
pub mod attempt;
mod canonical;
mod digest;
pub mod effort;
pub mod history;
mod identity;
pub mod limits;
pub mod message;
pub mod model_selection;
pub mod outcome;
pub mod result;
pub mod tool_call;
pub mod tool_error;
pub mod usage;

pub use address::{ModelAttemptKey, ModelMessageAddress, ToolCallAddress};
pub use attempt::{AttemptInterruption, AttemptRejection};
pub use canonical::canonical_json;
pub use digest::arguments_digest;
pub use effort::{Effort, ParseEffortError};
pub use history::ConversationItem;
pub use identity::{IdParseError, InvocationId, SessionId, TurnId};
pub use limits::{
    AttemptAssembly, LimitKind, MAX_ASSISTANT_CONTENT_BYTES, MAX_MODEL_ATTEMPT_BYTES,
    MAX_REASONING_CONTENT_BYTES, MAX_TOOL_CALL_ARGUMENTS_BYTES, MAX_TOOL_CALLS_PER_BATCH,
    validate_assistant_bounds,
};
pub use message::{
    MAX_OPAQUE_REPLAY_STATE_BYTES, MessagePart, MessageRole, MessageSemantic, NativeMessage,
    OpaqueProviderState, OpaqueReplayStateError, Protocol, ReplayOrigin, ToolCallState,
};
pub use model_selection::{
    MAX_MODEL_ID_BYTES, MAX_PROVIDER_ID_BYTES, ModelId, ModelIdError, ModelRef, ModelRefError,
    ProviderId, ProviderIdError,
};
pub use outcome::ToolResultOutcome;
pub use result::{
    IndeterminateCause, MAX_TOOL_CONTENT_BYTES, ResultPayload, StepContent, StoredToolResult,
    SuccessContent, tool_content_digest,
};
pub use tool_call::{
    ArgumentsDigestMismatch, BoundToolCall, RawToolCall, ToolCallValidation, UnboundToolCall,
    validate_tool_call_batch,
};
pub use tool_error::ToolError;
pub use usage::{
    CostEstimate, CostSummary, TokenSummary, TokenUsage, UsageCompleteness, UsageSummary,
};
