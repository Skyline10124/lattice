//! Error taxonomy for lattice-core.
//!
//! Defines a Rust `LatticeError` enum. Provides:
//!
//! - `LatticeError` enum: Rust-native error type with typed fields
//! - `ErrorClassifier`: HTTP status code → `LatticeError` classification
//!
//! # Error variants
//!
//! ```text
//! LatticeError
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

/// Native Rust error type representing all lattice-core error conditions.
///
/// Each variant carries structured fields describing the error.
#[derive(Error, Debug, Clone)]
pub enum LatticeError {
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

/// Classifies HTTP error responses into typed `LatticeError` variants.
///
/// Classifies HTTP error responses into typed `LatticeError` variants
/// based on status codes. Body-text pattern matching (context overflow
/// signals, billing vs rate-limit disambiguation) is handled entirely
/// by the Rust classifier — no Python-side delegation.
pub struct ErrorClassifier;

impl ErrorClassifier {
    /// Returns `true` if the error is retryable (rate limit or provider unavailable).
    pub fn is_retryable(error: &LatticeError) -> bool {
        matches!(
            error,
            LatticeError::RateLimit { .. } | LatticeError::ProviderUnavailable { .. }
        )
    }

    /// Classify an API error response by HTTP status code and body text.
    ///
    /// * `status_code` — HTTP status (0 if no response was received).
    /// * `response_body` — Raw response body text, used for pattern matching.
    /// * `provider` — Provider name (e.g. `"openai"`, `"anthropic"`).
    pub fn classify(status_code: u16, response_body: &str, provider: &str) -> LatticeError {
        let body_lower = response_body.to_lowercase();

        match status_code {
            // 429: Rate limit
            429 => {
                let retry_after = extract_retry_after(response_body);
                LatticeError::RateLimit {
                    retry_after,
                    provider: provider.to_string(),
                }
            }

            // 401/403: Authentication
            401 | 403 => LatticeError::Authentication {
                provider: provider.to_string(),
            },

            // 404: Model not found
            404 => {
                let model =
                    extract_model_from_body(response_body).unwrap_or_else(|| "unknown".to_string());
                LatticeError::ModelNotFound { model }
            }

            // 408/500/502/503/504: Provider unavailable
            408 | 500 | 502 | 503 | 504 => LatticeError::ProviderUnavailable {
                provider: provider.to_string(),
                reason: truncate_body(response_body),
            },

            // Everything else: pattern-match body for special cases
            _ => {
                if status_code == 400 {
                    if body_lower.contains("context_length_exceeded") {
                        let (tokens, limit) = extract_context_window_tokens(response_body);
                        LatticeError::ContextWindowExceeded { tokens, limit }
                    } else if body_lower.contains("overloaded_error") {
                        LatticeError::ProviderUnavailable {
                            provider: provider.to_string(),
                            reason: truncate_body(response_body),
                        }
                    } else if body_lower.contains("rate_limit_error") {
                        let retry_after = extract_retry_after(response_body);
                        LatticeError::RateLimit {
                            retry_after,
                            provider: provider.to_string(),
                        }
                    } else {
                        LatticeError::Network {
                            message: truncate_body(response_body),
                            status: Some(status_code),
                        }
                    }
                } else {
                    LatticeError::Network {
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
    let end = s
        .char_indices()
        .take_while(|&(i, ch)| i + ch.len_utf8() <= MAX_ERROR_BODY_LENGTH)
        .map(|(i, ch)| i + ch.len_utf8())
        .last()
        .unwrap_or(0);
    let mut truncated = String::with_capacity(MAX_ERROR_BODY_LENGTH + 16);
    truncated.push_str(&s[..end]);
    truncated.push_str("... (truncated)");
    truncated
}

// ── Helpers ─────────────────────────────────────────────────────────────

/// Extract (tokens_used, context_limit) from a context-window-exceeded error
/// body. Scans for patterns like "resulted in N tokens" and
/// "context length is N tokens". Returns (0, 0) when extraction fails.
fn extract_context_window_tokens(body: &str) -> (u32, u32) {
    let lower = body.to_lowercase();
    let mut tokens = 0u32;
    let mut limit = 0u32;

    // Look for "resulted in <N> tokens" (OpenAI style)
    if let Some(pos) = lower.find("resulted in") {
        let after = &lower[pos + "resulted in".len()..].trim_start();
        if let Some(n) = scan_first_number(after) {
            tokens = n;
        }
    }

    // Look for "context length is <N> tokens" (OpenAI style)
    if let Some(pos) = lower.find("context length is") {
        let after = &lower[pos + "context length is".len()..].trim_start();
        if let Some(n) = scan_first_number(after) {
            limit = n;
        }
    }

    (tokens, limit)
}

/// Scan the start of a string for an ASCII-digit number and parse it.
fn scan_first_number(s: &str) -> Option<u32> {
    let num_str: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    if num_str.is_empty() {
        None
    } else {
        num_str.parse().ok()
    }
}

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
                    .take_while(|c| c.is_ascii_digit() || *c == '.')
                    .collect();
                if let Ok(val) = num_str.parse::<f64>() {
                    if val >= 0.0 {
                        return Some(val);
                    }
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
            LatticeError::RateLimit {
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
            LatticeError::RateLimit {
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
            matches!(err, LatticeError::Authentication { .. }),
            "Expected Authentication, got {err:?}"
        );
    }

    #[test]
    fn test_classify_authentication_403() {
        let err = ErrorClassifier::classify(403, "forbidden", "google");
        assert!(
            matches!(err, LatticeError::Authentication { .. }),
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
            LatticeError::ModelNotFound { model } => {
                assert_eq!(model, "gpt-5");
            }
            _ => panic!("Expected ModelNotFound, got {err:?}"),
        }
    }

    #[test]
    fn test_classify_model_not_found_no_model_field() {
        let err = ErrorClassifier::classify(404, "not found", "openai");
        match err {
            LatticeError::ModelNotFound { model } => {
                assert_eq!(model, "unknown");
            }
            _ => panic!("Expected ModelNotFound, got {err:?}"),
        }
    }

    #[test]
    fn test_classify_provider_unavailable_500() {
        let err = ErrorClassifier::classify(500, "internal error", "openai");
        assert!(
            matches!(err, LatticeError::ProviderUnavailable { .. }),
            "Expected ProviderUnavailable, got {err:?}"
        );
    }

    #[test]
    fn test_classify_provider_unavailable_503() {
        let err = ErrorClassifier::classify(503, "service overloaded", "anthropic");
        assert!(
            matches!(err, LatticeError::ProviderUnavailable { .. }),
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
            matches!(err, LatticeError::ContextWindowExceeded { .. }),
            "Expected ContextWindowExceeded, got {err:?}"
        );
    }

    #[test]
    fn test_classify_network_error_unknown_status() {
        let err = ErrorClassifier::classify(0, "connection refused", "openai");
        match err {
            LatticeError::Network { message, status } => {
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
            LatticeError::Network { status, .. } => {
                assert_eq!(status, Some(418));
            }
            _ => panic!("Expected Network, got {err:?}"),
        }
    }

    #[test]
    fn test_classify_400_no_context_overflow() {
        let err = ErrorClassifier::classify(400, "bad request", "openai");
        assert!(
            matches!(err, LatticeError::Network { .. }),
            "Expected Network, got {err:?}"
        );
    }

    #[test]
    fn test_classify_400_overloaded_error() {
        let err = ErrorClassifier::classify(
            400,
            r#"{"error": {"type": "overloaded_error", "message": "Overloaded"}}"#,
            "anthropic",
        );
        assert!(
            matches!(err, LatticeError::ProviderUnavailable { .. }),
            "Expected ProviderUnavailable, got {err:?}"
        );
    }

    #[test]
    fn test_classify_400_rate_limit_error() {
        let err = ErrorClassifier::classify(
            400,
            r#"{"error": {"type": "rate_limit_error", "message": "Rate limited"}}"#,
            "anthropic",
        );
        assert!(
            matches!(err, LatticeError::RateLimit { .. }),
            "Expected RateLimit, got {err:?}"
        );
    }

    // ── Display tests ────────────────────────────────────────────────

    #[test]
    fn test_display_rate_limit() {
        let err = LatticeError::RateLimit {
            retry_after: Some(30.0),
            provider: "openai".into(),
        };
        let s = format!("{err}");
        assert!(s.contains("Rate limit"), "Display: {s}");
        assert!(s.contains("openai"), "Display: {s}");
    }

    #[test]
    fn test_display_authentication() {
        let err = LatticeError::Authentication {
            provider: "anthropic".into(),
        };
        let s = format!("{err}");
        assert!(s.contains("Authentication"), "Display: {s}");
        assert!(s.contains("anthropic"), "Display: {s}");
    }

    #[test]
    fn test_display_model_not_found() {
        let err = LatticeError::ModelNotFound {
            model: "gpt-5".into(),
        };
        let s = format!("{err}");
        assert!(s.contains("gpt-5"), "Display: {s}");
    }

    #[test]
    fn test_display_context_window_exceeded() {
        let err = LatticeError::ContextWindowExceeded {
            tokens: 100_000,
            limit: 128_000,
        };
        let s = format!("{err}");
        assert!(s.contains("100000"), "Display: {s}");
        assert!(s.contains("128000"), "Display: {s}");
    }

    #[test]
    fn test_display_provider_unavailable() {
        let err = LatticeError::ProviderUnavailable {
            provider: "openai".into(),
            reason: "down for maintenance".into(),
        };
        let s = format!("{err}");
        assert!(s.contains("openai"), "Display: {s}");
        assert!(s.contains("down for maintenance"), "Display: {s}");
    }

    #[test]
    fn test_display_tool_execution() {
        let err = LatticeError::ToolExecution {
            tool: "read_file".into(),
            message: "permission denied".into(),
        };
        let s = format!("{err}");
        assert!(s.contains("read_file"), "Display: {s}");
        assert!(s.contains("permission denied"), "Display: {s}");
    }

    #[test]
    fn test_display_streaming() {
        let err = LatticeError::Streaming {
            message: "connection lost".into(),
        };
        let s = format!("{err}");
        assert!(s.contains("connection lost"), "Display: {s}");
    }

    #[test]
    fn test_display_config() {
        let err = LatticeError::Config {
            message: "missing api key".into(),
        };
        let s = format!("{err}");
        assert!(s.contains("missing api key"), "Display: {s}");
    }

    #[test]
    fn test_display_network() {
        let err = LatticeError::Network {
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
