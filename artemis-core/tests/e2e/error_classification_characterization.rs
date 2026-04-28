//! Characterization tests for error classification in artemis-core.
//!
//! These tests capture the CURRENT behavior of BOTH ErrorClassifier
//! implementations before unification (T2). Do NOT modify implementation
//! code based on these tests — they document what IS, not what SHOULD BE.
//!
//! # Two ErrorClassifier implementations
//!
//! 1. `errors::ErrorClassifier` — richer, parses response body:
//!    - Extracts `retry_after` from JSON body on 429
//!    - Detects `context_length_exceeded` pattern in 400 responses
//!    - Extracts `model` from JSON body on 404
//!    - Fills `provider` field from third parameter
//!    - Uses lowercased body as `reason` for ProviderUnavailable
//!    - Third param is `provider: &str` (semantically correct)
//!
//! 2. `retry::ErrorClassifier` — simpler, status-code only:
//!    - No body parsing at all
//!    - No ContextWindowExceeded detection (400 → Network)
//!    - Uses third param as model name on 404 (not body extraction)
//!    - Leaves `provider` field empty (String::new())
//!    - Uses `"HTTP {code}"` as reason for ProviderUnavailable
//!    - Third param is `model: &str` (conceptual bug — should be provider)
//!    - Has `is_retryable()` method (errors:: doesn't)
//!
//! # Known bugs documented
//!
//! - L2: `extract_model_from_body` truncates on whitespace (e.g. "gpt 4" → "gpt")
//! - L3: `extract_retry_after` doesn't parse string-encoded numbers (e.g. `"30"` as string)
//! - retry:: classify third param `model: &str` is semantically wrong (should be provider)

use artemis_core::errors::{ArtemisError, ErrorClassifier as ErrorsClassifier};
use artemis_core::retry::ErrorClassifier as RetryClassifier;

// ════════════════════════════════════════════════════════════════════════
// Part 1: errors::ErrorClassifier classify() — per status code
// ════════════════════════════════════════════════════════════════════════

#[test]
fn errors_classify_429_rate_limit() {
    let err = ErrorsClassifier::classify(
        429,
        r#"{"error": "rate limit", "retry_after": 30}"#,
        "openai",
    );
    match err {
        ArtemisError::RateLimit {
            retry_after,
            provider,
        } => {
            assert_eq!(retry_after, Some(30.0), "errors:: extracts retry_after from body");
            assert_eq!(provider, "openai", "errors:: fills provider from third param");
        }
        _ => panic!("Expected RateLimit, got {err:?}"),
    }
}

#[test]
fn errors_classify_429_no_retry_after_in_body() {
    let err = ErrorsClassifier::classify(429, "too many requests", "anthropic");
    match err {
        ArtemisError::RateLimit {
            retry_after,
            provider,
        } => {
            assert_eq!(retry_after, None, "errors:: returns None when body has no retry_after");
            assert_eq!(provider, "anthropic");
        }
        _ => panic!("Expected RateLimit, got {err:?}"),
    }
}

#[test]
fn errors_classify_401_authentication() {
    let err = ErrorsClassifier::classify(401, "unauthorized", "anthropic");
    match err {
        ArtemisError::Authentication { provider } => {
            assert_eq!(provider, "anthropic", "errors:: fills provider on 401");
        }
        _ => panic!("Expected Authentication, got {err:?}"),
    }
}

#[test]
fn errors_classify_403_authentication() {
    let err = ErrorsClassifier::classify(403, "forbidden", "google");
    match err {
        ArtemisError::Authentication { provider } => {
            assert_eq!(provider, "google", "errors:: fills provider on 403");
        }
        _ => panic!("Expected Authentication, got {err:?}"),
    }
}

#[test]
fn errors_classify_404_model_from_body() {
    let err = ErrorsClassifier::classify(
        404,
        r#"{"error": "model not found", "model": "gpt-5"}"#,
        "openai",
    );
    match err {
        ArtemisError::ModelNotFound { model } => {
            assert_eq!(model, "gpt-5", "errors:: extracts model from JSON body");
        }
        _ => panic!("Expected ModelNotFound, got {err:?}"),
    }
}

#[test]
fn errors_classify_404_no_model_in_body() {
    let err = ErrorsClassifier::classify(404, "not found", "openai");
    match err {
        ArtemisError::ModelNotFound { model } => {
            assert_eq!(model, "unknown", "errors:: falls back to 'unknown' when body has no model");
        }
        _ => panic!("Expected ModelNotFound, got {err:?}"),
    }
}

#[test]
fn errors_classify_500_provider_unavailable() {
    let err = ErrorsClassifier::classify(500, "Internal Server Error", "openai");
    match err {
        ArtemisError::ProviderUnavailable { provider, reason } => {
            assert_eq!(provider, "openai", "errors:: fills provider on 500");
            assert_eq!(reason, "internal server error", "errors:: uses lowercased body as reason");
        }
        _ => panic!("Expected ProviderUnavailable, got {err:?}"),
    }
}

#[test]
fn errors_classify_502_provider_unavailable() {
    let err = ErrorsClassifier::classify(502, "Bad Gateway", "anthropic");
    match err {
        ArtemisError::ProviderUnavailable { provider, reason } => {
            assert_eq!(provider, "anthropic");
            assert_eq!(reason, "bad gateway", "errors:: lowercases body for reason");
        }
        _ => panic!("Expected ProviderUnavailable, got {err:?}"),
    }
}

#[test]
fn errors_classify_503_provider_unavailable() {
    let err = ErrorsClassifier::classify(503, "Service Overloaded", "groq");
    match err {
        ArtemisError::ProviderUnavailable { provider, reason } => {
            assert_eq!(provider, "groq");
            assert_eq!(reason, "service overloaded");
        }
        _ => panic!("Expected ProviderUnavailable, got {err:?}"),
    }
}

#[test]
fn errors_classify_400_context_window_exceeded() {
    let err = ErrorsClassifier::classify(
        400,
        r#"{"error": {"code": "context_length_exceeded"}}"#,
        "openai",
    );
    match err {
        ArtemisError::ContextWindowExceeded { tokens, limit } => {
            assert_eq!(tokens, 0, "errors:: ContextWindowExceeded tokens=0 (not extracted from body)");
            assert_eq!(limit, 0, "errors:: ContextWindowExceeded limit=0 (not extracted from body)");
        }
        _ => panic!("Expected ContextWindowExceeded, got {err:?}"),
    }
}

#[test]
fn errors_classify_400_no_context_overflow_is_network() {
    let err = ErrorsClassifier::classify(400, "bad request", "openai");
    match err {
        ArtemisError::Network { status, .. } => {
            assert_eq!(status, Some(400));
        }
        _ => panic!("Expected Network for 400 without context overflow, got {err:?}"),
    }
}

#[test]
fn errors_classify_other_status_is_network() {
    let err = ErrorsClassifier::classify(418, "I'm a teapot", "openai");
    match err {
        ArtemisError::Network { message, status } => {
            assert_eq!(status, Some(418));
            assert!(message.contains("teapot"), "errors:: Network message preserves original body");
        }
        _ => panic!("Expected Network, got {err:?}"),
    }
}

#[test]
fn errors_classify_status_0_is_network() {
    let err = ErrorsClassifier::classify(0, "connection refused", "openai");
    match err {
        ArtemisError::Network { status, .. } => {
            assert_eq!(status, Some(0), "errors:: status 0 maps to Network with status=0");
        }
        _ => panic!("Expected Network, got {err:?}"),
    }
}

// ════════════════════════════════════════════════════════════════════════
// Part 2: retry::ErrorClassifier classify() — per status code
// ════════════════════════════════════════════════════════════════════════

#[test]
fn retry_classify_429_rate_limit() {
    let err = RetryClassifier::classify(429, r#"{"retry_after": 30}"#, "openai");
    match err {
        ArtemisError::RateLimit {
            retry_after,
            provider,
        } => {
            assert_eq!(retry_after, None, "retry:: NEVER extracts retry_after from body");
            assert_eq!(provider, "", "retry:: leaves provider EMPTY (String::new())");
        }
        _ => panic!("Expected RateLimit, got {err:?}"),
    }
}

#[test]
fn retry_classify_401_authentication() {
    let err = RetryClassifier::classify(401, "unauthorized", "anthropic");
    match err {
        ArtemisError::Authentication { provider } => {
            assert_eq!(provider, "", "retry:: leaves provider EMPTY on 401");
        }
        _ => panic!("Expected Authentication, got {err:?}"),
    }
}

#[test]
fn retry_classify_403_authentication() {
    let err = RetryClassifier::classify(403, "forbidden", "google");
    match err {
        ArtemisError::Authentication { provider } => {
            assert_eq!(provider, "", "retry:: leaves provider EMPTY on 403");
        }
        _ => panic!("Expected Authentication, got {err:?}"),
    }
}

#[test]
fn retry_classify_404_model_from_param() {
    let err = RetryClassifier::classify(404, r#"{"model": "gpt-5"}"#, "unknown-model");
    match err {
        ArtemisError::ModelNotFound { model } => {
            assert_eq!(model, "unknown-model", "retry:: uses third param as model name, NOT body");
        }
        _ => panic!("Expected ModelNotFound, got {err:?}"),
    }
}

#[test]
fn retry_classify_500_provider_unavailable() {
    let err = RetryClassifier::classify(500, "Internal Server Error", "openai");
    match err {
        ArtemisError::ProviderUnavailable { provider, reason } => {
            assert_eq!(provider, "", "retry:: leaves provider EMPTY on 500");
            assert_eq!(reason, "HTTP 500", "retry:: uses formatted HTTP status as reason, not body");
        }
        _ => panic!("Expected ProviderUnavailable, got {err:?}"),
    }
}

#[test]
fn retry_classify_502_provider_unavailable() {
    let err = RetryClassifier::classify(502, "Bad Gateway", "anthropic");
    match err {
        ArtemisError::ProviderUnavailable { provider, reason } => {
            assert_eq!(provider, "");
            assert_eq!(reason, "HTTP 502", "retry:: reason is 'HTTP 502', ignores body");
        }
        _ => panic!("Expected ProviderUnavailable, got {err:?}"),
    }
}

#[test]
fn retry_classify_503_provider_unavailable() {
    let err = RetryClassifier::classify(503, "Service Overloaded", "groq");
    match err {
        ArtemisError::ProviderUnavailable { provider, reason } => {
            assert_eq!(provider, "");
            assert_eq!(reason, "HTTP 503", "retry:: reason is 'HTTP 503', ignores body");
        }
        _ => panic!("Expected ProviderUnavailable, got {err:?}"),
    }
}

#[test]
fn retry_classify_400_is_network_no_context_detection() {
    // DIVERGENCE: errors:: detects context_length_exceeded in 400 body,
    // retry:: does NOT — always maps 400 to Network
    let err = RetryClassifier::classify(
        400,
        r#"{"error": {"code": "context_length_exceeded"}}"#,
        "openai",
    );
    match err {
        ArtemisError::Network { status, message } => {
            assert_eq!(status, Some(400));
            assert!(message.contains("context_length_exceeded"), "retry:: treats context overflow as generic Network");
        }
        _ => panic!("Expected Network (not ContextWindowExceeded), got {err:?}"),
    }
}

#[test]
fn retry_classify_400_no_context_is_also_network() {
    let err = RetryClassifier::classify(400, "bad request", "openai");
    match err {
        ArtemisError::Network { status, .. } => {
            assert_eq!(status, Some(400));
        }
        _ => panic!("Expected Network, got {err:?}"),
    }
}

#[test]
fn retry_classify_other_status_is_network() {
    let err = RetryClassifier::classify(418, "I'm a teapot", "openai");
    match err {
        ArtemisError::Network { message, status } => {
            assert_eq!(status, Some(418));
            assert!(message.contains("418"), "retry:: Network message includes status code and body");
            assert!(message.contains("teapot"), "retry:: Network message includes body");
        }
        _ => panic!("Expected Network, got {err:?}"),
    }
}

#[test]
fn retry_classify_status_0_is_network() {
    let err = RetryClassifier::classify(0, "connection refused", "openai");
    match err {
        ArtemisError::Network { status, .. } => {
            assert_eq!(status, Some(0), "retry:: status 0 maps to Network with status=0");
        }
        _ => panic!("Expected Network, got {err:?}"),
    }
}

// ════════════════════════════════════════════════════════════════════════
// Part 3: Divergence documentation — side-by-side comparisons
// ════════════════════════════════════════════════════════════════════════

/// DIVERGENCE: errors:: extracts retry_after from body; retry:: does not.
#[test]
fn divergence_retry_after_extraction() {
    let body = r#"{"retry_after": 30}"#;

    let errors_err = ErrorsClassifier::classify(429, body, "openai");
    let retry_err = RetryClassifier::classify(429, body, "openai");

    // errors:: extracts it
    match errors_err {
        ArtemisError::RateLimit { retry_after, .. } => {
            assert_eq!(retry_after, Some(30.0), "errors:: extracts retry_after");
        }
        _ => panic!("Expected RateLimit from errors::"),
    }

    // retry:: does not
    match retry_err {
        ArtemisError::RateLimit { retry_after, .. } => {
            assert_eq!(retry_after, None, "retry:: does NOT extract retry_after");
        }
        _ => panic!("Expected RateLimit from retry::"),
    }
}

/// DIVERGENCE: errors:: detects context overflow via 400+body pattern;
/// retry:: maps all 400s to Network.
#[test]
fn divergence_context_overflow_detection() {
    let body = r#"{"error": {"code": "context_length_exceeded"}}"#;

    let errors_err = ErrorsClassifier::classify(400, body, "openai");
    let retry_err = RetryClassifier::classify(400, body, "openai");

    assert!(
        matches!(errors_err, ArtemisError::ContextWindowExceeded { .. }),
        "errors:: detects context overflow → ContextWindowExceeded"
    );
    assert!(
        matches!(retry_err, ArtemisError::Network { .. }),
        "retry:: does NOT detect context overflow → Network"
    );
}

/// DIVERGENCE: errors:: fills provider field; retry:: leaves it empty.
#[test]
fn divergence_provider_field() {
    let errors_err = ErrorsClassifier::classify(429, "rate limited", "anthropic");
    let retry_err = RetryClassifier::classify(429, "rate limited", "anthropic");

    match errors_err {
        ArtemisError::RateLimit { provider, .. } => {
            assert_eq!(provider, "anthropic", "errors:: fills provider from third param");
        }
        _ => panic!("Expected RateLimit"),
    }
    match retry_err {
        ArtemisError::RateLimit { provider, .. } => {
            assert_eq!(provider, "", "retry:: leaves provider as empty string");
        }
        _ => panic!("Expected RateLimit"),
    }
}

/// DIVERGENCE: errors:: uses provider param (semantically correct);
/// retry:: uses model param (conceptual error — the field should be provider).
#[test]
fn divergence_third_param_semantics() {
    // errors::classify(status, body, provider) — third param IS provider
    let err = ErrorsClassifier::classify(401, "unauthorized", "my-provider");
    match err {
        ArtemisError::Authentication { provider } => {
            assert_eq!(provider, "my-provider", "errors:: third param → provider field");
        }
        _ => panic!("Expected Authentication"),
    }

    // retry::classify(status, body, model) — third param is named 'model'
    // but it's used in ModelNotFound on 404, and ignored for other variants
    let err = RetryClassifier::classify(404, "not found", "my-model");
    match err {
        ArtemisError::ModelNotFound { model } => {
            assert_eq!(model, "my-model", "retry:: third param → model field on 404");
        }
        _ => panic!("Expected ModelNotFound"),
    }

    // For non-404 codes, retry:: ignores the third param entirely
    let err = RetryClassifier::classify(429, "rate limited", "this-should-be-provider");
    match err {
        ArtemisError::RateLimit { provider, .. } => {
            assert_eq!(provider, "", "retry:: ignores third param for 429, leaves provider empty");
        }
        _ => panic!("Expected RateLimit"),
    }
}

/// DIVERGENCE: retry:: has is_retryable() method; errors:: does not.
#[test]
fn divergence_is_retryable_only_in_retry() {
    // retry::ErrorClassifier::is_retryable exists
    let rate_limit = RetryClassifier::classify(429, "", "x");
    assert!(RetryClassifier::is_retryable(&rate_limit), "retry:: is_retryable(RateLimit) = true");

    let auth = RetryClassifier::classify(401, "", "x");
    assert!(!RetryClassifier::is_retryable(&auth), "retry:: is_retryable(Authentication) = false");

    // errors::ErrorClassifier has no is_retryable method at all
    // (This is a structural observation, not a runtime assertion)
}

/// DIVERGENCE: errors:: uses lowercased body as ProviderUnavailable reason;
/// retry:: uses "HTTP {code}" format.
#[test]
fn divergence_provider_unavailable_reason() {
    let errors_err = ErrorsClassifier::classify(503, "Service Overloaded", "groq");
    let retry_err = RetryClassifier::classify(503, "Service Overloaded", "groq");

    match errors_err {
        ArtemisError::ProviderUnavailable { reason, .. } => {
            assert_eq!(reason, "service overloaded", "errors:: reason = lowercased body");
        }
        _ => panic!("Expected ProviderUnavailable"),
    }
    match retry_err {
        ArtemisError::ProviderUnavailable { reason, .. } => {
            assert_eq!(reason, "HTTP 503", "retry:: reason is formatted HTTP status");
        }
        _ => panic!("Expected ProviderUnavailable"),
    }
}

/// DIVERGENCE: errors:: extracts model from JSON body on 404;
/// retry:: uses third param as model name on 404.
#[test]
fn divergence_404_model_source() {
    let body = r#"{"error": "not found", "model": "gpt-5"}"#;

    let errors_err = ErrorsClassifier::classify(404, body, "some-provider");
    let retry_err = RetryClassifier::classify(404, body, "some-model");

    match errors_err {
        ArtemisError::ModelNotFound { model } => {
            assert_eq!(model, "gpt-5", "errors:: extracts model from JSON body");
        }
        _ => panic!("Expected ModelNotFound"),
    }
    match retry_err {
        ArtemisError::ModelNotFound { model } => {
            assert_eq!(model, "some-model", "retry:: uses third param as model name");
        }
        _ => panic!("Expected ModelNotFound"),
    }
}

/// DIVERGENCE: errors:: Network message preserves original body text;
/// retry:: Network message is formatted as "HTTP {code}: {body}".
#[test]
fn divergence_network_message_format() {
    let errors_err = ErrorsClassifier::classify(418, "I'm a teapot", "openai");
    let retry_err = RetryClassifier::classify(418, "I'm a teapot", "openai");

    match errors_err {
        ArtemisError::Network { message, .. } => {
            assert_eq!(message, "I'm a teapot", "errors:: Network message = original body");
        }
        _ => panic!("Expected Network"),
    }
    match retry_err {
        ArtemisError::Network { message, .. } => {
            assert_eq!(message, "HTTP 418: I'm a teapot", "retry:: Network message format");
        }
        _ => panic!("Expected Network"),
    }
}

// ════════════════════════════════════════════════════════════════════════
// Part 4: retry::ErrorClassifier is_retryable() — per ArtemisError variant
// ════════════════════════════════════════════════════════════════════════

#[test]
fn is_retryable_rate_limit_yes() {
    let err = ArtemisError::RateLimit {
        retry_after: None,
        provider: "openai".into(),
    };
    assert!(RetryClassifier::is_retryable(&err), "RateLimit IS retryable");
}

#[test]
fn is_retryable_provider_unavailable_yes() {
    let err = ArtemisError::ProviderUnavailable {
        provider: "openai".into(),
        reason: "overloaded".into(),
    };
    assert!(RetryClassifier::is_retryable(&err), "ProviderUnavailable IS retryable");
}

#[test]
fn is_retryable_authentication_no() {
    let err = ArtemisError::Authentication {
        provider: "openai".into(),
    };
    assert!(!RetryClassifier::is_retryable(&err), "Authentication is NOT retryable");
}

#[test]
fn is_retryable_model_not_found_no() {
    let err = ArtemisError::ModelNotFound {
        model: "gpt-5".into(),
    };
    assert!(!RetryClassifier::is_retryable(&err), "ModelNotFound is NOT retryable");
}

#[test]
fn is_retryable_context_window_exceeded_no() {
    let err = ArtemisError::ContextWindowExceeded {
        tokens: 100_000,
        limit: 128_000,
    };
    assert!(!RetryClassifier::is_retryable(&err), "ContextWindowExceeded is NOT retryable");
}

#[test]
fn is_retryable_tool_execution_no() {
    let err = ArtemisError::ToolExecution {
        tool: "read_file".into(),
        message: "permission denied".into(),
    };
    assert!(!RetryClassifier::is_retryable(&err), "ToolExecution is NOT retryable");
}

#[test]
fn is_retryable_streaming_no() {
    let err = ArtemisError::Streaming {
        message: "connection lost".into(),
    };
    assert!(!RetryClassifier::is_retryable(&err), "Streaming is NOT retryable");
}

#[test]
fn is_retryable_config_no() {
    let err = ArtemisError::Config {
        message: "missing api key".into(),
    };
    assert!(!RetryClassifier::is_retryable(&err), "Config is NOT retryable");
}

#[test]
fn is_retryable_network_no() {
    let err = ArtemisError::Network {
        message: "timeout".into(),
        status: Some(504),
    };
    assert!(!RetryClassifier::is_retryable(&err), "Network is NOT retryable");
}

// ════════════════════════════════════════════════════════════════════════
// Part 5: extract_retry_after() — tested indirectly via errors::classify
// ════════════════════════════════════════════════════════════════════════
//
// Note: extract_retry_after is a private function in errors.rs.
// We test it indirectly through errors::ErrorClassifier::classify().

#[test]
fn retry_after_numeric_integer() {
    let err = ErrorsClassifier::classify(
        429,
        r#"{"retry_after": 30}"#,
        "openai",
    );
    match err {
        ArtemisError::RateLimit { retry_after, .. } => {
            assert_eq!(retry_after, Some(30.0), "Parses integer retry_after");
        }
        _ => panic!("Expected RateLimit"),
    }
}

#[test]
fn retry_after_numeric_float() {
    let err = ErrorsClassifier::classify(
        429,
        r#"{"retry_after": 5.5}"#,
        "openai",
    );
    match err {
        ArtemisError::RateLimit { retry_after, .. } => {
            assert_eq!(retry_after, Some(5.5), "Parses float retry_after");
        }
        _ => panic!("Expected RateLimit"),
    }
}

#[test]
fn retry_after_hyphenated_key() {
    let err = ErrorsClassifier::classify(
        429,
        r#"{"retry-after": 20}"#,
        "openai",
    );
    match err {
        ArtemisError::RateLimit { retry_after, .. } => {
            assert_eq!(retry_after, Some(20.0), "Parses retry-after (hyphenated) key");
        }
        _ => panic!("Expected RateLimit"),
    }
}

#[test]
fn retry_after_unquoted_key() {
    // Body contains retry_after without JSON quotes around key
    let err = ErrorsClassifier::classify(
        429,
        "retry_after: 10",
        "openai",
    );
    match err {
        ArtemisError::RateLimit { retry_after, .. } => {
            assert_eq!(retry_after, Some(10.0), "Parses unquoted retry_after key");
        }
        _ => panic!("Expected RateLimit"),
    }
}

#[test]
fn retry_after_not_present() {
    let err = ErrorsClassifier::classify(
        429,
        r#"{"error": "too many requests"}"#,
        "openai",
    );
    match err {
        ArtemisError::RateLimit { retry_after, .. } => {
            assert_eq!(retry_after, None, "Returns None when no retry_after in body");
        }
        _ => panic!("Expected RateLimit"),
    }
}

#[test]
fn retry_after_string_encoded_number_bug() {
    // BUG (L3): extract_retry_after does NOT parse string-encoded numbers.
    // When retry_after is "30" (string), the parser finds the key but
    // the take_while skips the quote character, so it can't parse the value.
    let err = ErrorsClassifier::classify(
        429,
        r#"{"retry_after": "30"}"#,
        "openai",
    );
    match err {
        ArtemisError::RateLimit { retry_after, .. } => {
            assert_eq!(retry_after, None, "BUG L3: string-encoded '30' is NOT parsed");
        }
        _ => panic!("Expected RateLimit"),
    }
}

// ════════════════════════════════════════════════════════════════════════
// Part 6: extract_model_from_body() — tested indirectly via errors::classify
// ════════════════════════════════════════════════════════════════════════
//
// Note: extract_model_from_body is a private function in errors.rs.
// We test it indirectly through errors::ErrorClassifier::classify() on 404.

#[test]
fn model_from_body_standard_json() {
    let err = ErrorsClassifier::classify(
        404,
        r#"{"error": "not found", "model": "gpt-5"}"#,
        "openai",
    );
    match err {
        ArtemisError::ModelNotFound { model } => {
            assert_eq!(model, "gpt-5", "Extracts model from standard JSON");
        }
        _ => panic!("Expected ModelNotFound"),
    }
}

#[test]
fn model_from_body_no_model_key() {
    let err = ErrorsClassifier::classify(
        404,
        r#"{"error": "not found"}"#,
        "openai",
    );
    match err {
        ArtemisError::ModelNotFound { model } => {
            assert_eq!(model, "unknown", "Falls back to 'unknown' when no model in body");
        }
        _ => panic!("Expected ModelNotFound"),
    }
}

#[test]
fn model_from_body_null_model() {
    // When body has "model": null, extract_model_from_body returns None
    // because it checks model != "null"
    let err = ErrorsClassifier::classify(
        404,
        r#"{"model": null}"#,
        "openai",
    );
    match err {
        ArtemisError::ModelNotFound { model } => {
            assert_eq!(model, "unknown", "null model value → 'unknown' fallback");
        }
        _ => panic!("Expected ModelNotFound"),
    }
}

#[test]
fn model_from_body_whitespace_truncation_bug() {
    // BUG (L2): extract_model_from_body stops on whitespace.
    // A model name like "gpt 4 turbo" in the body gets truncated to "gpt"
    // because the take_while excludes space characters.
    let err = ErrorsClassifier::classify(
        404,
        r#"{"model": "gpt 4 turbo"}"#,
        "openai",
    );
    match err {
        ArtemisError::ModelNotFound { model } => {
            assert_eq!(model, "gpt", "BUG L2: whitespace in model name causes truncation");
        }
        _ => panic!("Expected ModelNotFound"),
    }
}

#[test]
fn model_from_body_hyphenated_name() {
    // Hyphenated model names work fine (no whitespace)
    let err = ErrorsClassifier::classify(
        404,
        r#"{"model": "claude-sonnet-4-6"}"#,
        "openai",
    );
    match err {
        ArtemisError::ModelNotFound { model } => {
            assert_eq!(model, "claude-sonnet-4-6", "Hyphenated model names work");
        }
        _ => panic!("Expected ModelNotFound"),
    }
}

#[test]
fn model_from_body_case_insensitive_key() {
    // extract_model_from_body lowercases the body before finding "model" key,
    // but the extracted value also comes from the lowercased body.
    let err = ErrorsClassifier::classify(
        404,
        r#"{"Model": "GPT-5"}"#,
        "openai",
    );
    match err {
        ArtemisError::ModelNotFound { model } => {
            assert_eq!(model, "gpt-5", "Model name extracted from lowercased body (key and value both lowered)");
        }
        _ => panic!("Expected ModelNotFound"),
    }
}

// ════════════════════════════════════════════════════════════════════════
// Part 7: Status code coverage matrix — both classifiers
// ════════════════════════════════════════════════════════════════════════
//
// These tests verify the complete status code → variant mapping for both
// classifiers in a systematic way.

#[test]
fn status_code_matrix_both_classifiers() {
    // (status_code, body, expected_errors_variant, expected_retry_variant)
    let cases: Vec<(u16, &str, &str, &str)> = vec![
        (429, r#"{"retry_after": 30}"#, "RateLimit", "RateLimit"),
        (401, "unauthorized", "Authentication", "Authentication"),
        (403, "forbidden", "Authentication", "Authentication"),
        (404, r#"{"model": "x"}"#, "ModelNotFound", "ModelNotFound"),
        (500, "error", "ProviderUnavailable", "ProviderUnavailable"),
        (502, "error", "ProviderUnavailable", "ProviderUnavailable"),
        (503, "error", "ProviderUnavailable", "ProviderUnavailable"),
        (400, r#"{"error": {"code": "context_length_exceeded"}}"#, "ContextWindowExceeded", "Network"),
        (400, "bad request", "Network", "Network"),
        (418, "teapot", "Network", "Network"),
        (0, "connection refused", "Network", "Network"),
    ];

    for (status, body, errors_expected, retry_expected) in &cases {
        let errors_err = ErrorsClassifier::classify(*status, body, "test-provider");
        let retry_err = RetryClassifier::classify(*status, body, "test-model");

        let errors_name = variant_name(&errors_err);
        let retry_name = variant_name(&retry_err);

        assert_eq!(
            errors_name, *errors_expected,
            "errors:: classify({status}) → {errors_name}, expected {errors_expected}"
        );
        assert_eq!(
            retry_name, *retry_expected,
            "retry:: classify({status}) → {retry_name}, expected {retry_expected}"
        );
    }
}

/// Helper: get the variant name of an ArtemisError as a string.
fn variant_name(err: &ArtemisError) -> &'static str {
    match err {
        ArtemisError::RateLimit { .. } => "RateLimit",
        ArtemisError::Authentication { .. } => "Authentication",
        ArtemisError::ModelNotFound { .. } => "ModelNotFound",
        ArtemisError::ProviderUnavailable { .. } => "ProviderUnavailable",
        ArtemisError::ContextWindowExceeded { .. } => "ContextWindowExceeded",
        ArtemisError::ToolExecution { .. } => "ToolExecution",
        ArtemisError::Streaming { .. } => "Streaming",
        ArtemisError::Config { .. } => "Config",
        ArtemisError::Network { .. } => "Network",
    }
}
