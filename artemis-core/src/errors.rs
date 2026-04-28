//! Error taxonomy for artemis-core.
//!
//! Defines a Rust `ArtemisError` enum that maps to a Python exception
//! hierarchy via PyO3. Provides:
//!
//! - `py_exc` module: PyO3 exception classes (ArtemisError base + 9 subclasses)
//! - `ArtemisError` enum: Rust-native error type with typed fields
//! - `From<ArtemisError> for PyErr`: automatic conversion to Python exceptions
//! - `ErrorClassifier`: HTTP status code → `ArtemisError` classification
//!
//! # Python exception hierarchy
//!
//! ```text
//! Exception
//!   └─ ArtemisError (base)
//!        ├─ RateLimitError          (has .retry_after, .provider)
//!        ├─ AuthenticationError     (has .provider)
//!        ├─ ModelNotFoundError      (has .model)
//!        ├─ ProviderUnavailableError (has .provider, .reason)
//!        ├─ ContextWindowExceededError (has .tokens, .limit)
//!        ├─ ToolExecutionError      (has .tool, .message)
//!        ├─ StreamingError          (has .message)
//!        ├─ ConfigError             (has .message)
//!        └─ NetworkError            (has .message, .status)
//! ```

use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use thiserror::Error;

// ── PyO3 Python exception classes ──────────────────────────────────────
//
// These live in a nested module to avoid a naming collision between the
// Rust `ArtemisError` enum (below) and the `create_exception!` ZST marker
// type, which Rustc sees as two types with the same name in the same scope.
//
// From Python: `import artemis_core; artemis_core.RateLimitError`
// From Rust:   `errors::py_exc::RateLimitError`
pub mod py_exc {
    use super::*;

    create_exception!(artemis_core, ArtemisError, PyException);
    create_exception!(artemis_core, RateLimitError, ArtemisError);
    create_exception!(artemis_core, AuthenticationError, ArtemisError);
    create_exception!(artemis_core, ModelNotFoundError, ArtemisError);
    create_exception!(artemis_core, ProviderUnavailableError, ArtemisError);
    create_exception!(artemis_core, ContextWindowExceededError, ArtemisError);
    create_exception!(artemis_core, ToolExecutionError, ArtemisError);
    create_exception!(artemis_core, StreamingError, ArtemisError);
    create_exception!(artemis_core, ConfigError, ArtemisError);
    create_exception!(artemis_core, NetworkError, ArtemisError);
}

// ── Rust error enum ────────────────────────────────────────────────────

/// Native Rust error type representing all artemis-core error conditions.
///
/// Each variant maps to a specific Python exception subclass via
/// `From<ArtemisError> for PyErr`, carrying structured fields that
/// become Python exception attributes.
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

// ── Conversion: ArtemisError → PyErr ───────────────────────────────────

impl From<ArtemisError> for PyErr {
    fn from(err: ArtemisError) -> PyErr {
        match err {
            ArtemisError::RateLimit {
                retry_after,
                provider,
            } => Python::try_attach(|py| {
                let msg = format!("Rate limit exceeded for provider '{provider}'");
                let py_err = PyErr::new::<py_exc::RateLimitError, _>((msg,));
                let instance = py_err.value(py);
                let _ = instance.setattr("retry_after", retry_after);
                let _ = instance.setattr("provider", provider);
                py_err
            })
            .expect("Python interpreter not initialized"),

            ArtemisError::Authentication { provider } => Python::try_attach(|py| {
                let msg = format!("Authentication failed for provider '{provider}'");
                let py_err = PyErr::new::<py_exc::AuthenticationError, _>((msg,));
                let _ = py_err.value(py).setattr("provider", provider);
                py_err
            })
            .expect("Python interpreter not initialized"),

            ArtemisError::ModelNotFound { model } => Python::try_attach(|py| {
                let msg = format!("Model '{model}' not found");
                let py_err = PyErr::new::<py_exc::ModelNotFoundError, _>((msg,));
                let _ = py_err.value(py).setattr("model", model);
                py_err
            })
            .expect("Python interpreter not initialized"),

            ArtemisError::ProviderUnavailable { provider, reason } => Python::try_attach(|py| {
                let msg = format!("Provider '{provider}' unavailable: {reason}");
                let py_err = PyErr::new::<py_exc::ProviderUnavailableError, _>((msg,));
                let instance = py_err.value(py);
                let _ = instance.setattr("provider", provider);
                let _ = instance.setattr("reason", reason);
                py_err
            })
            .expect("Python interpreter not initialized"),

            ArtemisError::ContextWindowExceeded { tokens, limit } => Python::try_attach(|py| {
                let msg = format!("Context window exceeded: {tokens} tokens (limit {limit})");
                let py_err = PyErr::new::<py_exc::ContextWindowExceededError, _>((msg,));
                let instance = py_err.value(py);
                let _ = instance.setattr("tokens", tokens);
                let _ = instance.setattr("limit", limit);
                py_err
            })
            .expect("Python interpreter not initialized"),

            ArtemisError::ToolExecution { tool, message } => Python::try_attach(|py| {
                let msg = format!("Tool '{tool}' execution failed: {message}");
                let py_err = PyErr::new::<py_exc::ToolExecutionError, _>((msg,));
                let instance = py_err.value(py);
                let _ = instance.setattr("tool", tool);
                let _ = instance.setattr("message", message);
                py_err
            })
            .expect("Python interpreter not initialized"),

            ArtemisError::Streaming { message } => Python::try_attach(|py| {
                let py_err = PyErr::new::<py_exc::StreamingError, _>((message.clone(),));
                let _ = py_err.value(py).setattr("message", message);
                py_err
            })
            .expect("Python interpreter not initialized"),

            ArtemisError::Config { message } => Python::try_attach(|py| {
                let py_err = PyErr::new::<py_exc::ConfigError, _>((message.clone(),));
                let _ = py_err.value(py).setattr("message", message);
                py_err
            })
            .expect("Python interpreter not initialized"),

            ArtemisError::Network { message, status } => Python::try_attach(|py| {
                let py_err = PyErr::new::<py_exc::NetworkError, _>((message.clone(),));
                let instance = py_err.value(py);
                let _ = instance.setattr("message", message);
                let _ = instance.setattr("status", status);
                py_err
            })
            .expect("Python interpreter not initialized"),
        }
    }
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

            // 500/502/503: Provider unavailable
            500 | 502 | 503 => ArtemisError::ProviderUnavailable {
                provider: provider.to_string(),
                reason: body_lower,
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
                        message: response_body.to_string(),
                        status: Some(status_code),
                    }
                }
            }
        }
    }
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
                let after_colon = after_key[colon_pos + 1..].trim();
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

/// Extract the Python class name from a Bound exception object.
#[cfg(test)]
fn exc_class_name(val: &Bound<'_, pyo3::exceptions::PyBaseException>) -> String {
    val.getattr("__class__")
        .and_then(|c| c.getattr("__name__"))
        .and_then(|n| n.extract::<String>())
        .unwrap_or_default()
}

/// Extract MRO names (as strings) from a Bound exception object.
#[cfg(test)]
fn exc_mro_names(val: &Bound<'_, pyo3::exceptions::PyBaseException>) -> Vec<String> {
    let mro = val
        .getattr("__class__")
        .and_then(|c| c.getattr("__mro__"))
        .unwrap();
    let count = mro.len().unwrap_or(0);
    let mut names = Vec::new();
    for i in 0..count {
        if let Ok(base) = mro.get_item(i) {
            if let Ok(name) = base.getattr("__name__").and_then(|n| n.extract::<String>()) {
                names.push(name);
            }
        }
    }
    names
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::py_exc;
    use super::*;

    /// Acquire the Python GIL, initializing the interpreter if needed.
    /// This is required because `From<ArtemisError> for PyErr` calls
    /// `Python::try_attach` which returns `None` if Python is uninitialized.
    #[cfg(test)]
    /// Run a closure with the Python GIL acquired (for roundtrip tests).
    /// These tests are `#[ignore]` by default and run only via Python
    /// integration test harness (`pytest` / `python -c`).
    fn with_python<F, R>(f: F) -> R
    where
        F: for<'py> FnOnce(Python<'py>) -> R,
    {
        // In practice this is invoked from Python tests where the
        // interpreter is already initialized and GIL is held.
        Python::try_attach(f).expect("Python not initialized")
    }

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

    // ── PyErr roundtrip tests ────────────────────────────────────────

    #[test]
    #[ignore = "requires Python runtime (run via Python integration tests)"]
    fn test_py_err_rate_limit_roundtrip() {
        let err = ArtemisError::RateLimit {
            retry_after: Some(30.0),
            provider: "openai".into(),
        };
        let py_err: PyErr = err.into();
        with_python(|py| {
            let val = py_err.value(py);
            let class_name = exc_class_name(val);
            assert!(
                class_name.contains("RateLimitError"),
                "Expected RateLimitError, got {class_name}"
            );

            let s = val.str().unwrap();
            let msg = s.to_string_lossy();
            assert!(msg.contains("Rate limit"), "Message: {msg}");
            assert!(msg.contains("openai"), "Message: {msg}");

            let ra = val.getattr("retry_after").unwrap();
            assert_eq!(ra.extract::<Option<f64>>().unwrap(), Some(30.0));

            let pr = val.getattr("provider").unwrap();
            assert_eq!(pr.extract::<String>().unwrap(), "openai");

            let mro = exc_mro_names(val);
            assert!(
                mro.contains(&"ArtemisError".to_string()),
                "MRO should contain ArtemisError, got: {mro:?}"
            );
        });
    }

    #[test]
    #[ignore = "requires Python runtime (run via Python integration tests)"]
    fn test_py_err_authentication_roundtrip() {
        let err = ArtemisError::Authentication {
            provider: "anthropic".into(),
        };
        let py_err: PyErr = err.into();
        with_python(|py| {
            let val = py_err.value(py);

            let class_name = exc_class_name(val);
            assert!(
                class_name.contains("AuthenticationError"),
                "Expected AuthenticationError, got {class_name}"
            );

            let pr = val.getattr("provider").unwrap();
            assert_eq!(pr.extract::<String>().unwrap(), "anthropic");
        });
    }

    #[test]
    #[ignore = "requires Python runtime (run via Python integration tests)"]
    fn test_py_err_context_window_roundtrip() {
        let err = ArtemisError::ContextWindowExceeded {
            tokens: 100_000,
            limit: 128_000,
        };
        let py_err: PyErr = err.into();
        with_python(|py| {
            let val = py_err.value(py);

            let class_name = exc_class_name(val);
            assert!(
                class_name.contains("ContextWindowExceededError"),
                "Expected ContextWindowExceededError, got {class_name}"
            );

            let tokens_attr = val.getattr("tokens").unwrap();
            assert_eq!(tokens_attr.extract::<u32>().unwrap(), 100_000);
            let limit_attr = val.getattr("limit").unwrap();
            assert_eq!(limit_attr.extract::<u32>().unwrap(), 128_000);
        });
    }

    #[test]
    #[ignore = "requires Python runtime (run via Python integration tests)"]
    fn test_py_err_model_not_found_roundtrip() {
        let err = ArtemisError::ModelNotFound {
            model: "gpt-5".into(),
        };
        let py_err: PyErr = err.into();
        with_python(|py| {
            let val = py_err.value(py);

            let class_name = exc_class_name(val);
            assert!(
                class_name.contains("ModelNotFoundError"),
                "Expected ModelNotFoundError, got {class_name}"
            );

            let model_attr = val.getattr("model").unwrap();
            assert_eq!(model_attr.extract::<String>().unwrap(), "gpt-5");
        });
    }

    #[test]
    #[ignore = "requires Python runtime (run via Python integration tests)"]
    fn test_py_err_network_roundtrip() {
        let err = ArtemisError::Network {
            message: "timeout".into(),
            status: Some(504),
        };
        let py_err: PyErr = err.into();
        with_python(|py| {
            let val = py_err.value(py);

            let class_name = exc_class_name(val);
            assert!(
                class_name.contains("NetworkError"),
                "Expected NetworkError, got {class_name}"
            );

            let status_attr = val.getattr("status").unwrap();
            assert_eq!(status_attr.extract::<Option<u16>>().unwrap(), Some(504));
        });
    }

    #[test]
    #[ignore = "requires Python runtime (run via Python integration tests)"]
    fn test_py_err_all_exception_types_roundtrip() {
        with_python(|py| {
            let cases: Vec<(ArtemisError, &str)> = vec![
                (
                    ArtemisError::RateLimit {
                        retry_after: None,
                        provider: "x".into(),
                    },
                    "RateLimitError",
                ),
                (
                    ArtemisError::Authentication {
                        provider: "x".into(),
                    },
                    "AuthenticationError",
                ),
                (
                    ArtemisError::ModelNotFound { model: "x".into() },
                    "ModelNotFoundError",
                ),
                (
                    ArtemisError::ProviderUnavailable {
                        provider: "x".into(),
                        reason: "x".into(),
                    },
                    "ProviderUnavailableError",
                ),
                (
                    ArtemisError::ContextWindowExceeded {
                        tokens: 0,
                        limit: 0,
                    },
                    "ContextWindowExceededError",
                ),
                (
                    ArtemisError::ToolExecution {
                        tool: "x".into(),
                        message: "x".into(),
                    },
                    "ToolExecutionError",
                ),
                (
                    ArtemisError::Streaming {
                        message: "x".into(),
                    },
                    "StreamingError",
                ),
                (
                    ArtemisError::Config {
                        message: "x".into(),
                    },
                    "ConfigError",
                ),
                (
                    ArtemisError::Network {
                        message: "x".into(),
                        status: None,
                    },
                    "NetworkError",
                ),
            ];

            for (variant, expected_class) in cases {
                let py_err: PyErr = variant.into();
                let val = py_err.value(py);
                let name = exc_class_name(val);
                assert!(
                    name.contains(expected_class),
                    "Expected {expected_class}, got {name}"
                );
                let mro = exc_mro_names(val);
                assert!(
                    mro.contains(&"ArtemisError".to_string()),
                    "{expected_class} MRO should contain ArtemisError, got: {mro:?}"
                );
            }
        });
    }

    #[test]
    #[ignore = "requires Python runtime (run via Python integration tests)"]
    fn test_exception_hierarchy() {
        with_python(|py| {
            // Check that all exception types share the ArtemisError base.
            let artemis_type: Bound<'_, pyo3::types::PyType> =
                py.get_type::<py_exc::ArtemisError>();
            let exc_type: Bound<'_, pyo3::types::PyType> =
                py.get_type::<pyo3::exceptions::PyException>();

            // Verify: issubclass(ArtemisError, Exception)
            let is_sub = artemis_type
                .call_method1("__subclasscheck__", (exc_type,))
                .unwrap()
                .extract::<bool>()
                .unwrap();
            assert!(is_sub, "ArtemisError should be subclass of Exception");

            // Verify: issubclass(RateLimitError, ArtemisError)
            let rate_limit_type: Bound<'_, pyo3::types::PyType> =
                py.get_type::<py_exc::RateLimitError>();
            let is_sub = rate_limit_type
                .call_method1("__subclasscheck__", (artemis_type,))
                .unwrap()
                .extract::<bool>()
                .unwrap();
            assert!(is_sub, "RateLimitError should be subclass of ArtemisError");
        });
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
