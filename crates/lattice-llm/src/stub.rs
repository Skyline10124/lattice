// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Deterministic stub LLM implementation
//!
//! Provides fake streaming responses that require no credentials, HTTP clients,
//! or real model routing. Output is derived deterministically from the user prompt.

use lattice_conversation::{MessageRole, MessageSemantic};

use crate::router::ModelRouter;
use crate::types::{ChatRequest, LlmEvent};
use futures::stream::{self, Stream};
use std::pin::Pin;
use thiserror::Error;

/// Errors that can occur during LLM operations.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum LlmError {
    #[error("unknown model alias: '{0}'")]
    UnknownModel(String),

    #[error("credential error: {message}")]
    Credential { message: String },

    #[error("configuration error: {message}")]
    Config { message: String },

    #[error("DNS error: {message}")]
    Dns { message: String },

    #[error("transport error: {message}")]
    Transport { message: String, retryable: bool },

    #[error("HTTP request timed out: {message}")]
    Timeout { message: String },

    /// 非 2xx 响应。`message` 是已有界的一行诊断，如 `HTTP 429 rate_limit_exceeded: …`。
    #[error("{message}")]
    ProviderHttp {
        status_code: u16,
        message: String,
        retryable: bool,
        /// provider 通过 `retry-after`/`retry-after-ms` 给出的建议等待。
        retry_after: Option<std::time::Duration>,
    },

    #[error("HTTP response read error: {message}")]
    StreamRead { message: String },

    #[error("protocol limit exceeded: {message}")]
    ProtocolLimit { message: String },

    #[error("provider projection error: {0}")]
    Projection(#[from] crate::protocol::projection::ProjectionError),

    #[error("LLM error: {0}")]
    Other(String),
}

impl LlmError {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::UnknownModel(_) => "unknown_model",
            Self::Credential { .. } => "credential",
            Self::Config { .. } => "config",
            Self::Dns { .. } => "dns",
            Self::Transport { .. } => "transport",
            Self::Timeout { .. } => "timeout",
            Self::ProviderHttp { .. } => "provider_http",
            Self::StreamRead { .. } => "stream_read",
            Self::ProtocolLimit { .. } => "protocol_limit",
            Self::Projection(_) => "projection",
            Self::Other(_) => "other",
        }
    }

    pub fn retryable(&self) -> bool {
        match self {
            Self::Transport { retryable, .. } | Self::ProviderHttp { retryable, .. } => *retryable,
            Self::Timeout { .. } => true,
            _ => false,
        }
    }

    pub fn status_code(&self) -> Option<u16> {
        match self {
            Self::ProviderHttp { status_code, .. } => Some(*status_code),
            _ => None,
        }
    }

    pub fn retry_after(&self) -> Option<std::time::Duration> {
        match self {
            Self::ProviderHttp { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

pub type LlmStream = Pin<Box<dyn Stream<Item = LlmEvent> + Send>>;

fn native_text(
    message: &lattice_conversation::NativeMessage<lattice_conversation::BoundToolCall>,
) -> String {
    message
        .parts
        .iter()
        .filter_map(|part| match &part.semantic {
            MessageSemantic::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}
/// A deterministic stub LLM that returns fake chat and stream responses.
#[derive(Clone, Debug, Default)]
pub struct StubLlm {
    pub router: ModelRouter,
}

impl StubLlm {
    pub fn new() -> Self {
        StubLlm::default()
    }

    /// Return a fake streaming response as a boxed stream of [`LlmEvent`] values.
    pub fn stream(&self, request: ChatRequest) -> Result<LlmStream, LlmError> {
        let _resolved = self.router.resolve(&request.target)?;

        let last_user_msg = request
            .messages
            .iter()
            .rev()
            .find_map(|item| match item {
                lattice_conversation::ConversationItem::Message(message)
                    if message.role == MessageRole::User =>
                {
                    Some(native_text(message))
                }
                _ => None,
            })
            .unwrap_or_default();

        let token_content = if last_user_msg.is_empty() {
            "stub response".to_string()
        } else {
            format!("stub response: {}", last_user_msg)
        };

        let events: Vec<LlmEvent> = vec![
            LlmEvent::Started,
            LlmEvent::Token {
                content: token_content.clone(),
            },
            LlmEvent::MessageComplete {
                message: lattice_conversation::NativeMessage::new(
                    MessageRole::Assistant,
                    None,
                    vec![lattice_conversation::MessagePart::semantic_only(
                        MessageSemantic::Text {
                            text: token_content,
                        },
                    )],
                ),
            },
            LlmEvent::Done {
                finish_reason: "stop".to_string(),
                usage: None,
            },
        ];

        Ok(Box::pin(stream::iter(events)))
    }
}

/// Convenience function: stream a completion through the real client.
pub async fn stream(request: ChatRequest) -> Result<LlmStream, LlmError> {
    let resolved = ModelRouter::new().resolve(&request.target)?;
    crate::client::dispatch_stream(request, resolved).await
}
