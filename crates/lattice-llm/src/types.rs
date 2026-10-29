// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! These types define the direct wire contract for LATTICE LLM calls
//! and are independent of `lattice-core`.

use lattice_conversation::{ConversationItem, TokenUsage};
use serde::{Deserialize, Serialize};

/// A tool definition providing a function specification to the model.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    /// JSON schema for the tool parameters.
    pub parameters: serde_json::Value,
}

impl ToolDefinition {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        ToolDefinition {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }
}

/// A chat completion request to be sent to an LLM.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChatRequest {
    /// Durable provider-neutral history. Provider wire is projected exactly once
    /// by the selected provider builder.
    pub messages: Vec<ConversationItem>,
    pub tools: Vec<ToolDefinition>,
    pub target: ModelTarget,
    /// 作为 `x-client-request-id` 发出，取 SessionId，便于在 provider 侧关联同一会话的请求。
    pub client_request_id: Option<String>,
}

impl ChatRequest {
    pub fn new(messages: impl Into<Vec<ConversationItem>>, target: ModelTarget) -> Self {
        ChatRequest {
            messages: messages.into(),
            tools: Vec::new(),
            target,
            client_request_id: None,
        }
    }
}

/// provider 的错误分类独立于已收到的 delta 数量。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderFailure {
    pub message: String,
    pub retryable: bool,
}

/// 同一次累积产生的实时投影和唯一终态。
#[derive(Clone, Debug, PartialEq)]
pub enum LlmEvent {
    Started,
    Delta(crate::LiveDelta),
    Finished {
        outcome: crate::ReplyOutcome,
        report: AttemptReport,
    },
}

/// `Finished` 携带结束原因、规范化用量与可选费用估算。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AttemptReport {
    pub finish_reason: Option<String>,
    pub usage: Option<TokenUsage>,
    pub estimated_cost_usd: Option<f64>,
}

/// 请求明确给出 provider id 与模型 id，不提供默认模型。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelTarget {
    pub model: String,
    pub provider: String,
}

impl ModelTarget {
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
        }
    }
}
