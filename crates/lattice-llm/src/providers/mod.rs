//! Concrete provider modules for v1.1 LLM execution.
//!
//! Each provider module owns request JSON construction, SSE parsing,
//! and auth header logic for a specific provider family.
//! No provider registry, plugin trait, or `lattice-core` dependency exists here.

pub mod anthropic;
pub mod gemini;
pub mod openai;

use crate::types::LlmEvent;

/// Parse `data` into a JSON value, returning the value or an error event.
pub(crate) fn parse_json_data(provider: &str, data: &str) -> Result<serde_json::Value, LlmEvent> {
    let trimmed = data.trim();
    if trimmed.is_empty() || trimmed == "[DONE]" {
        return Err(LlmEvent::Started); // sentinel: caller should skip empty chunks
    }
    serde_json::from_str(trimmed).map_err(|e| LlmEvent::Error {
        message: format!("{} SSE JSON parse error: {}", provider, e),
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
///
/// Multiple system messages are merged with `"\n\n"`.  If `slot` is
/// `None`, it is initialised to `text` without a separator.
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
pub(crate) fn u32_field(obj: &serde_json::Value, key: &str) -> u32 {
    obj.get(key).and_then(|v| v.as_u64()).unwrap_or(0) as u32
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
