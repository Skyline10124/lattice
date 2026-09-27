// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 持久结构化工具结果；provider 文本只能在投影边界派生。

use crate::tool_call::hex_digest;
use crate::{BoundToolCall, NativeMessage, ToolResultOutcome};
use lancet_protocol::{CepError, CepErrorCode, InvocationId, Retriability};
use serde::{Deserialize, Serialize};

pub const MAX_TOOL_CONTENT_BYTES: usize = 65_536;

/// 保持既有结果摘要的域分离和规范字节长度编码。
pub fn tool_content_digest(canonical_content: &str) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"lattice-tool-content-digest-v1\0");
    hasher.update((canonical_content.len() as u64).to_be_bytes());
    hasher.update(canonical_content.as_bytes());
    hasher.finalize().into()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotExecutedReason {
    BeforeDispatch,
    CancelledBeforeDispatch,
    DeadlineBeforeDispatch,
    PrecedingInvocationIndeterminate,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndeterminateReason {
    MayHaveOccurred,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RejectedReason {
    pub code: CepErrorCode,
    pub retriability: Retriability,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelledReason {
    DuringDispatch,
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredToolResult {
    pub invocation_id: InvocationId,
    pub name: String,
    pub payload: ResultPayload,
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "outcome",
    content = "payload",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ResultPayload {
    Succeeded(SuccessContent),
    Failed(Box<CepError>),
    Rejected(Box<CepError>),
    Cancelled(CancelledReason),
    NotExecuted(NotExecutedReason),
    Indeterminate(IndeterminateReason),
}
impl ResultPayload {
    pub fn outcome(&self) -> ToolResultOutcome {
        match self {
            Self::Succeeded(_) => ToolResultOutcome::Succeeded,
            Self::Failed(_) => ToolResultOutcome::Failed,
            Self::Rejected(_) => ToolResultOutcome::Rejected,
            Self::Cancelled(_) => ToolResultOutcome::Cancelled,
            Self::NotExecuted(_) => ToolResultOutcome::NotExecuted,
            Self::Indeterminate(_) => ToolResultOutcome::Indeterminate,
        }
    }
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SuccessContent {
    Json {
        value: serde_json::Value,
        #[serde(with = "hex_digest")]
        digest: [u8; 32],
        byte_length: u64,
    },
    OutputTooLarge {
        #[serde(with = "hex_digest")]
        digest: [u8; 32],
        byte_length: u64,
    },
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepContent {
    pub assistant: NativeMessage<BoundToolCall>,
    pub results: Vec<StoredToolResult>,
}

macro_rules! redacted_debug { ($($ty:ty),+ $(,)?) => {$ (
    impl std::fmt::Debug for $ty {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(concat!(stringify!($ty), " { … }"))
        }
    }
)+}; }
redacted_debug!(StoredToolResult, ResultPayload, SuccessContent, StepContent);
