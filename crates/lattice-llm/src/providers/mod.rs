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

/// Append `text` to an optional system-message accumulator.
pub(crate) fn push_system_text(slot: &mut Option<String>, text: &str) {
    match slot {
        Some(ref mut existing) => {
            existing.push_str("\n\n");
            existing.push_str(text);
        }
        None => *slot = Some(text.to_string()),
    }
}

/// Read a `u32` field from a JSON object, defaulting to 0.
///
/// Saturates at `u32::MAX` instead of silently truncating to avoid
/// surprising usage values when a provider returns an unexpectedly large
/// number.
pub(crate) fn u32_field(obj: &serde_json::Value, key: &str) -> u32 {
    obj.get(key)
        .and_then(|v| v.as_u64())
        .map(|v| u32::try_from(v).unwrap_or(u32::MAX))
        .unwrap_or(0)
}

use crate::types::TokenUsage;

/// Build a `TokenUsage` from a JSON object using provider-specific keys.
pub(crate) fn token_usage_from_keys(
    obj: &serde_json::Value,
    prompt_key: &str,
    completion_key: &str,
    total_key: &str,
) -> TokenUsage {
    TokenUsage {
        prompt_tokens: u32_field(obj, prompt_key),
        completion_tokens: u32_field(obj, completion_key),
        total_tokens: u32_field(obj, total_key),
    }
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
