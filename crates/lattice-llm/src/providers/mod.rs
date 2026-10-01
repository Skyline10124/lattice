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
        .map_err(|e| LlmEvent::error(format!("{provider} SSE JSON parse error: {e}"), false))
}

/// Extract an API-level error event from a response JSON value.
/// Returns `None` if `root` does not contain an error payload.
pub(crate) fn extract_api_error(
    provider: lattice_conversation::ProviderOrigin,
    root: &serde_json::Value,
) -> Option<LlmEvent> {
    use lattice_conversation::ProviderOrigin;
    let error = root.get("error")?;
    let message = error
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown provider error");
    let retryable = match provider {
        ProviderOrigin::OpenAiCompatible => matches!(
            error["type"].as_str(),
            Some("server_error" | "rate_limit_error")
        ),
        ProviderOrigin::Anthropic => matches!(
            error["type"].as_str(),
            Some("overloaded_error" | "api_error" | "rate_limit_error")
        ),
        ProviderOrigin::Gemini => matches!(
            error["status"].as_str(),
            Some("UNAVAILABLE" | "INTERNAL" | "RESOURCE_EXHAUSTED")
        ),
    };
    Some(LlmEvent::error(format!("API error: {message}"), retryable))
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
        Err(crate::provider_projection::ProjectionError::OutputLimitExceeded {
            interruption,
            ..
        }) => LlmEvent::OutputLimit { interruption },
        Err(error) => LlmEvent::error(error.to_string(), false),
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
    fn streaming_retryability_uses_provider_fields_not_message() {
        use lattice_conversation::ProviderOrigin::*;
        for (provider, error, retryable) in [
            (
                OpenAiCompatible,
                serde_json::json!({"type":"server_error", "message":"failure"}),
                true,
            ),
            (
                Anthropic,
                serde_json::json!({"type":"overloaded_error", "message":"failure"}),
                true,
            ),
            (
                Gemini,
                serde_json::json!({"status":"UNAVAILABLE", "message":"failure"}),
                true,
            ),
            (
                OpenAiCompatible,
                serde_json::json!({"type":"invalid_request_error", "message":"server_error retry me"}),
                false,
            ),
            (
                Anthropic,
                serde_json::json!({"message":"overloaded_error"}),
                false,
            ),
            (
                Gemini,
                serde_json::json!({"status":"INVALID_ARGUMENT", "message":"UNAVAILABLE"}),
                false,
            ),
        ] {
            let LlmEvent::Error { failure } =
                extract_api_error(provider, &serde_json::json!({"error":error})).unwrap()
            else {
                panic!("typed failure");
            };
            assert_eq!(failure.retryable, retryable);
        }
    }
    #[test]
    fn parse_invalid_json_returns_error() {
        let result = parse_json_data("TestProvider", "{invalid}");
        assert!(matches!(result, Err(LlmEvent::Error { .. })));
    }
}
