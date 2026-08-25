// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Concrete provider modules for LLM execution.
//!
//! Each provider module owns request JSON construction, SSE parsing,
//! and auth header logic for a specific provider family.
//! No provider registry, plugin trait, or `lattice-core` dependency exists here.

pub mod anthropic;
pub mod gemini;
pub mod openai;

use crate::types::LlmEvent;

/// Parse `data` into a JSON value.
///
/// Returns:
/// - `Ok(Some(value))` for parsed JSON
/// - `Ok(None)` for empty data or `[DONE]`
/// - `Err(LlmEvent::Error)` for parse errors
pub(crate) fn parse_json_data(
    provider: &str,
    data: &str,
) -> Result<Option<serde_json::Value>, LlmEvent> {
    let trimmed = data.trim();

    if trimmed.is_empty() || trimmed == "[DONE]" {
        return Ok(None);
    }

    serde_json::from_str(trimmed)
        .map(Some)
        .map_err(|e| LlmEvent::Error {
            message: format!("{provider} SSE JSON parse error: {e}"),
        })
}

/// Extract an API-level error event from a response JSON value.
/// Returns `None` if `root` does not contain an error payload.
pub(crate) fn extract_api_error(root: &serde_json::Value) -> Option<LlmEvent> {
    root.get("error")
        .and_then(|err| err.get("message"))
        .and_then(|m| m.as_str())
        .map(|message| LlmEvent::Error {
            message: format!("API error: {}", message),
        })
}

pub(crate) fn projection_result_event(
    result: Result<
        lattice_conversation::NativeMessage<lattice_conversation::UnboundToolCall>,
        crate::provider_projection::ProjectionError,
    >,
) -> LlmEvent {
    match result {
        Ok(message) => LlmEvent::MessageComplete { message },
        Err(crate::provider_projection::ProjectionError::InvalidToolCall(validation)) => {
            LlmEvent::MessageRejected {
                rejection: lattice_conversation::AttemptRejection::InvalidToolCall { validation },
            }
        }
        Err(error) => LlmEvent::Error {
            message: error.to_string(),
        },
    }
}

pub(crate) fn complete_message_event(
    provider: lattice_conversation::ProviderOrigin,
    wire: &serde_json::Value,
) -> LlmEvent {
    projection_result_event(crate::provider_projection::wire_to_native(provider, wire))
}

#[cfg(test)]
mod parse_json_data_tests {
    use super::*;

    #[test]
    fn parse_valid_json_returns_some() {
        let result = parse_json_data("TestProvider", r#"{"key": "value"}"#);
        assert!(matches!(result, Ok(Some(v)) if v.get("key").unwrap() == "value"));
    }

    #[test]
    fn parse_empty_string_returns_none() {
        let result = parse_json_data("TestProvider", "");
        assert!(matches!(result, Ok(None)));
    }

    #[test]
    fn parse_whitespace_only_returns_none() {
        let result = parse_json_data("TestProvider", "   \t\n  ");
        assert!(matches!(result, Ok(None)));
    }

    #[test]
    fn parse_done_sentinel_returns_none() {
        let result = parse_json_data("TestProvider", "[DONE]");
        assert!(matches!(result, Ok(None)));
    }

    #[test]
    fn parse_done_with_whitespace_returns_none() {
        let result = parse_json_data("TestProvider", "  [DONE]  ");
        assert!(matches!(result, Ok(None)));
    }

    #[test]
    fn parse_invalid_json_returns_error() {
        let result = parse_json_data("TestProvider", "{invalid}");
        assert!(matches!(result, Err(LlmEvent::Error { .. })));
    }
}
