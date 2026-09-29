// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 持久结构化工具结果；provider 文本只能在投影边界派生。

use crate::tool_call::hex_digest;
use crate::{BoundToolCall, NativeMessage, ToolResultOutcome};
use lancet_protocol::{CepError, InvocationId};
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

/// 已越过 Started 却没有确定结果的原因；外部效果未知。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndeterminateCause {
    /// Step 停止或进程退出时尚未取得结果，由关闭事务补写。
    NoResult,
    /// LANCET 返回 `CallCancelled`。
    Cancelled,
    /// LANCET 返回 `OutcomeIndeterminate`。
    Reported,
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
    /// LANCET 返回 JSON。
    Succeeded(SuccessContent),
    /// LANCET 返回 CepError；策略或校验拒绝是其中的错误族。
    Failed(Box<CepError>),
    /// 从未交给 LANCET。不带原因：为什么停记在 Step 的结束原因里。
    NotExecuted,
    /// 已越过 Started，效果未知。
    Indeterminate(IndeterminateCause),
}
impl ResultPayload {
    pub fn outcome(&self) -> ToolResultOutcome {
        match self {
            Self::Succeeded(_) => ToolResultOutcome::Succeeded,
            Self::Failed(_) => ToolResultOutcome::Failed,
            Self::NotExecuted => ToolResultOutcome::NotExecuted,
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
