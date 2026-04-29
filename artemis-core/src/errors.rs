//! Error taxonomy for artemis-core.
//!
//! Defines a Rust `ArtemisError` enum. Provides:
//!
//! - `ArtemisError` enum: Rust-native error type with typed fields
//! - `ErrorClassifier`: HTTP status code → `ArtemisError` classification
//!
//! # Error variants
//!
//! ```text
//! ArtemisError
//!   ├─ RateLimit          (.retry_after, .provider)
//!   ├─ Authentication     (.provider)
//!   ├─ ModelNotFound      (.model)
//!   ├─ ProviderUnavailable (.provider, .reason)
//!   ├─ ContextWindowExceeded (.tokens, .limit)
//!   ├─ ToolExecution      (.tool, .message)
//!   ├─ Streaming          (.message)
//!   ├─ Config             (.message)
//!   └─ Network            (.message, .status)
//! ```

use thiserror::Error;

// ── Rust error enum ────────────────────────────────────────────────────

/// Native Rust error type representing all artemis-core error conditions.
///
/// Each variant carries structured fields describing the error.
#[derive(Error, Debug, Clone)]
pub enum ArtemisError {
    /// Provider rate-limited the request.
    #[error("Rate limit exceeded for provider '{provider}'")]
    RateLimit {
        /// Seconds after which a retry may succeed (if provided).
        retry_after: Option<f64>,
        /// The provider that returned the rate limit.
        provider: String,
    },

    /// Authentication / authorization failure.
    #[error("Authentication failed for provider '{provider}'")]
    Authentication {
        /// The provider that rejected the credentials.
        provider: String,
    },

    /// The requested model was not found or is unavailable.
    #[error("Model '{model}' not found")]
    ModelNotFound {
        /// The model identifier that was not found.
        model: String,
    },

    /// The provider is temporarily unavailable.
    #[error("Provider '{provider}' unavailable: {reason}")]
    ProviderUnavailable {
        /// The provider that is down.
        provider: String,
        /// Human-readable reason for the outage.
        reason: String,
    },

    /// The context window was exceeded.
    #[error("Context window exceeded: {tokens} tokens (limit {limit})")]
    ContextWindowExceeded {
        /// Number of tokens in the current context.
        tokens: u32,
        /// Maximum allowed context length for the model.
        limit: u32,
    },

    /// A tool call failed during execution.
    #[error("Tool '{tool}' execution failed: {message}")]
    ToolExecution {
        /// Name of the tool that failed.
        tool: String,
        /// Error message from the tool.
        message: String,
    },

    /// Streaming error during response generation.
    #[error("Streaming error: {message}")]
    Streaming {
        /// Description of the streaming failure.
        message: String,
    },

    /// Configuration error.
    #[error("Configuration error: {message}")]
    Config {
        /// Description of the configuration problem.
        message: String,
    },

    /// Generic network / transport error.
    #[error("Network error: {message}")]
    Network {
        /// Description of the network failure.
        message: String,
        /// HTTP status code, if available.
        status: Option<u16>,
    },
}

// ── Error classifier ───────────────────────────────────────────────────

/// Classifies HTTP error responses into typed `ArtemisError` variants.
///
/// Mirrors the priority-ordered classification logic from the Python
/// `hermes-agent/agent/error_classifier.py` for the common status-code
/// codepath. Body-text pattern matching (context overflow signals,
/// billing vs rate-limit disambiguation) is delegated to the Python side
/// for now — the Rust classifier handles the deterministic status-code
/// mapping.
pub struct ErrorClassifier;

impl ErrorClassifier {
    /// Returns `true` if the error is retryable (rate limit or provider unavailable).
    pub fn is_retryable(error: &ArtemisError) -> bool {
        matches!(
            error,
            ArtemisError::RateLimit { .. } | ArtemisError::ProviderUnavailable { .. }
        )
    }

    /// Classify an API error response by HTTP status code and body text.
    ///
    /// * `status_code` — HTTP status (0 if no response was received).
    /// * `response_body` — Raw response body text, used for pattern matching.
    /// * `provider` — Provider name (e.g. `"openai"`, `"anthropic"`).
    pub fn classify(status_code: u16, response_body: &str, provider: &str) -> ArtemisError {
        let body_lower = response_body.to_lowercase();

        match status_code {
            // 429: Rate limit
            429 => {
                let retry_after = extract_retry_after(response_body);
                ArtemisError::RateLimit {
                    retry_after,
                    provider: provider.to_string(),
                }
            }

            // 401/403: Authentication
            401 | 403 => ArtemisError::Authentication {
                provider: provider.to_string(),
            },

            // 404: Model not found
            404 => {
                let model =
                    extract_model_from_body(response_body).unwrap_or_else(|| "unknown".to_string());
                ArtemisError::ModelNotFound { model }
            }

            // 408/500/502/503/504: Provider unavailable
            408 | 500 | 502 | 503 | 504 => ArtemisError::ProviderUnavailable {
                provider: provider.to_string(),
                reason: truncate_body(&body_lower),
            },

            // Everything else: pattern-match body for special cases
            _ => {
                if status_code == 400 && body_lower.contains("context_length_exceeded") {
                    ArtemisError::ContextWindowExceeded {
                        tokens: 0,
                        limit: 0,
                    }
                } else {
                    ArtemisError::Network {
                        message: truncate_body(response_body),
                        status: Some(status_code),
                    }
                }
            }
        }
    }
}

// ── Error body size limiting ────────────────────────────────────────────

/// Maximum number of bytes to keep from an error response body.
/// Bodies longer than this are truncated to prevent memory exhaustion.
const MAX_ERROR_BODY_LENGTH: usize = 8192;

/// Truncate a string to `MAX_ERROR_BODY_LENGTH` bytes, appending
/// `... (truncated)` if it was cut short.
///
/// Classification (pattern matching) should be done on the full body
/// *before* calling this — this is purely a storage/display limit.
fn truncate_body(s: &str) -> String {
    if s.len() <= MAX_ERROR_BODY_LENGTH {
        return s.to_string();
    }
    let mut truncated = String::with_capacity(MAX_ERROR_BODY_LENGTH + 16);
    truncated.push_str(&s[..MAX_ERROR_BODY_LENGTH]);
    truncated.push_str("... (truncated)");
    truncated
}

// ── Helpers ─────────────────────────────────────────────────────────────

/// Extract `retry_after` seconds from a response body.
fn extract_retry_after(body: &str) -> Option<f64> {
    let body_lower = body.to_lowercase();

    for key in &[
        "\"retry_after\"",
        "\"retry-after\"",
        "retry_after",
        "retry-after",
    ] {
        if let Some(pos) = body_lower.find(key) {
            let after_key = &body_lower[pos + key.len()..];
            if let Some(colon_pos) = after_key.find(':') {
                let after_colon = after_key[colon_pos + 1..].trim().trim_matches('"');
                let num_str: String = after_colon
                    .chars()
                    .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
                    .collect();
                if let Ok(val) = num_str.parse::<f64>() {
                    return Some(val);
                }
            }
        }
    }
    None
}

/// Extract the model name from a JSON error response body.
fn extract_model_from_body(body: &str) -> Option<String> {
    let lower = body.to_lowercase();
    if let Some(pos) = lower.find("\"model\"") {
        let after = &lower[pos + "\"model\"".len()..];
        if let Some(colon_pos) = after.find(':') {
            let after_colon = after[colon_pos + 1..].trim();
            let trimmed = after_colon.trim_start_matches('"');
            let model: String = trimmed
                .chars()
                .take_while(|c| *c != '"' && *c != ',' && *c != '}' && *c != '\n' && *c != ' ')
                .collect();
            if !model.is_empty() && model != "null" {
                return Some(model);
            }
        }
    }
    None
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── ErrorClassifier tests ────────────────────────────────────────

    #[test]
    fn test_classify_rate_limit_with_retry_after() {
        let err = ErrorClassifier::classify(
            429,
            r#"{"error": "rate limit", "retry_after": 30}"#,
            "openai",
        );
        match err {
            ArtemisError::RateLimit {
                retry_after,
                provider,
            } => {
                assert_eq!(retry_after, Some(30.0));
                assert_eq!(provider, "openai");
            }
            _ => panic!("Expected RateLimit, got {err:?}"),
        }
    }

    #[test]
    fn test_classify_rate_limit_no_retry_after() {
        let err = ErrorClassifier::classify(429, "too many requests", "anthropic");
        match err {
            ArtemisError::RateLimit {
                retry_after,
                provider,
            } => {
                assert_eq!(retry_after, None);
                assert_eq!(provider, "anthropic");
            }
            _ => panic!("Expected RateLimit, got {err:?}"),
        }
    }

    #[test]
    fn test_classify_authentication_401() {
        let err = ErrorClassifier::classify(401, "unauthorized", "anthropic");
        assert!(
            matches!(err, ArtemisError::Authentication { .. }),
            "Expected Authentication, got {err:?}"
        );
    }

    #[test]
    fn test_classify_authentication_403() {
        let err = ErrorClassifier::classify(403, "forbidden", "google");
        assert!(
            matches!(err, ArtemisError::Authentication { .. }),
            "Expected Authentication, got {err:?}"
        );
    }

    #[test]
    fn test_classify_model_not_found() {
        let err = ErrorClassifier::classify(
            404,
            r#"{"error": "model not found", "model": "gpt-5"}"#,
            "openai",
        );
        match err {
            ArtemisError::ModelNotFound { model } => {
                assert_eq!(model, "gpt-5");
            }
            _ => panic!("Expected ModelNotFound, got {err:?}"),
        }
    }

    #[test]
    fn test_classify_model_not_found_no_model_field() {
        let err = ErrorClassifier::classify(404, "not found", "openai");
        match err {
            ArtemisError::ModelNotFound { model } => {
                assert_eq!(model, "unknown");
            }
            _ => panic!("Expected ModelNotFound, got {err:?}"),
        }
    }

    #[test]
    fn test_classify_provider_unavailable_500() {
        let err = ErrorClassifier::classify(500, "internal error", "openai");
        assert!(
            matches!(err, ArtemisError::ProviderUnavailable { .. }),
            "Expected ProviderUnavailable, got {err:?}"
        );
    }

    #[test]
    fn test_classify_provider_unavailable_503() {
        let err = ErrorClassifier::classify(503, "service overloaded", "anthropic");
        assert!(
            matches!(err, ArtemisError::ProviderUnavailable { .. }),
            "Expected ProviderUnavailable, got {err:?}"
        );
    }

    #[test]
    fn test_classify_context_window_exceeded() {
        let err = ErrorClassifier::classify(
            400,
            r#"{"error": {"code": "context_length_exceeded"}}"#,
            "openai",
        );
        assert!(
            matches!(err, ArtemisError::ContextWindowExceeded { .. }),
            "Expected ContextWindowExceeded, got {err:?}"
        );
    }

    #[test]
    fn test_classify_network_error_unknown_status() {
        let err = ErrorClassifier::classify(0, "connection refused", "openai");
        match err {
            ArtemisError::Network { message, status } => {
                assert_eq!(status, Some(0));
                assert!(message.contains("connection refused"));
            }
            _ => panic!("Expected Network, got {err:?}"),
        }
    }

    #[test]
    fn test_classify_network_error_418() {
        let err = ErrorClassifier::classify(418, "I'm a teapot", "openai");
        match err {
            ArtemisError::Network { status, .. } => {
                assert_eq!(status, Some(418));
            }
            _ => panic!("Expected Network, got {err:?}"),
        }
    }

    #[test]
    fn test_classify_400_no_context_overflow() {
        let err = ErrorClassifier::classify(400, "bad request", "openai");
        assert!(
            matches!(err, ArtemisError::Network { .. }),
            "Expected Network, got {err:?}"
        );
    }

    // ── Display tests ────────────────────────────────────────────────

    #[test]
    fn test_display_rate_limit() {
        let err = ArtemisError::RateLimit {
            retry_after: Some(30.0),
            provider: "openai".into(),
        };
        let s = format!("{err}");
        assert!(s.contains("Rate limit"), "Display: {s}");
        assert!(s.contains("openai"), "Display: {s}");
    }

    #[test]
    fn test_display_authentication() {
        let err = ArtemisError::Authentication {
            provider: "anthropic".into(),
        };
        let s = format!("{err}");
        assert!(s.contains("Authentication"), "Display: {s}");
        assert!(s.contains("anthropic"), "Display: {s}");
    }

    #[test]
    fn test_display_model_not_found() {
        let err = ArtemisError::ModelNotFound {
            model: "gpt-5".into(),
        };
        let s = format!("{err}");
        assert!(s.contains("gpt-5"), "Display: {s}");
    }

    #[test]
    fn test_display_context_window_exceeded() {
        let err = ArtemisError::ContextWindowExceeded {
            tokens: 100_000,
            limit: 128_000,
        };
        let s = format!("{err}");
        assert!(s.contains("100000"), "Display: {s}");
        assert!(s.contains("128000"), "Display: {s}");
    }

    #[test]
    fn test_display_provider_unavailable() {
        let err = ArtemisError::ProviderUnavailable {
            provider: "openai".into(),
            reason: "down for maintenance".into(),
        };
        let s = format!("{err}");
        assert!(s.contains("openai"), "Display: {s}");
        assert!(s.contains("down for maintenance"), "Display: {s}");
    }

    #[test]
    fn test_display_tool_execution() {
        let err = ArtemisError::ToolExecution {
            tool: "read_file".into(),
            message: "permission denied".into(),
        };
        let s = format!("{err}");
        assert!(s.contains("read_file"), "Display: {s}");
        assert!(s.contains("permission denied"), "Display: {s}");
    }

    #[test]
    fn test_display_streaming() {
        let err = ArtemisError::Streaming {
            message: "connection lost".into(),
        };
        let s = format!("{err}");
        assert!(s.contains("connection lost"), "Display: {s}");
    }

    #[test]
    fn test_display_config() {
        let err = ArtemisError::Config {
            message: "missing api key".into(),
        };
        let s = format!("{err}");
        assert!(s.contains("missing api key"), "Display: {s}");
    }

    #[test]
    fn test_display_network() {
        let err = ArtemisError::Network {
            message: "timeout".into(),
            status: Some(504),
        };
        let s = format!("{err}");
        assert!(s.contains("Network"), "Display: {s}");
    }

    // ── Helper function tests ────────────────────────────────────────

    #[test]
    fn test_extract_retry_after_json() {
        let body = r#"{"error": "rate limit", "retry_after": 30}"#;
        assert_eq!(extract_retry_after(body), Some(30.0));
    }

    #[test]
    fn test_extract_retry_after_json_float() {
        let body = r#"{"retry_after": 5.5}"#;
        assert_eq!(extract_retry_after(body), Some(5.5));
    }

    #[test]
    fn test_extract_retry_after_not_found() {
        let body = r#"{"error": "server error"}"#;
        assert_eq!(extract_retry_after(body), None);
    }

    #[test]
    fn test_extract_model_from_body() {
        let body = r#"{"error": "not found", "model": "gpt-4"}"#;
        assert_eq!(extract_model_from_body(body), Some("gpt-4".into()));
    }

    #[test]
    fn test_extract_model_from_body_no_model() {
        let body = r#"{"error": "not found"}"#;
        assert_eq!(extract_model_from_body(body), None);
    }
}
