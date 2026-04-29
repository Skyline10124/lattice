//! Regression tests for Wave 4 (edge cases) and Wave 5 (low-priority) bug fixes.
//!
//! These tests verify that fixes remain in place and that the public API
//! surfaces behave as expected. Tests are organized by bug class per the
//! code review report.
//!
//! ## Wave 4 bug classes
//! - T26 (M3): Catalog::get() returns Result (no panic)
//! - T29 (M8): Error response body truncated
//! - T30 (M9): base_url URL format validation
//!
//! ## Wave 5 bug classes
//! - T31 (L5): Anthropic SSE error event handling
//! - T31 (L1): OpenAI SSE delta tracking
//! - T32 (L2+L3): Model body extraction + retry_after parsing
//! - T33 (L4): HTTP status code classification matrix
//! - T34 (L7): resolve_permissive normalization
//! - T35 (L9): ChatRequest model field consistency

use std::collections::HashMap;
use std::env;

use artemis_core::catalog::{ApiProtocol, ResolvedModel};
use artemis_core::errors::ArtemisError;
use artemis_core::errors::ErrorClassifier;
use artemis_core::provider::ChatRequest;
use artemis_core::router::{self, ModelRouter};
use artemis_core::streaming::{AnthropicSseParser, OpenAiSseParser, SseParser, StreamEvent};
use artemis_core::types::{Message, Role, ToolDefinition};
use serde_json::json;

// ── Helpers ────────────────────────────────────────────────────────────────

fn save_env(key: &str) -> Option<String> {
    env::var(key).ok()
}

fn restore_env(key: &str, prev: Option<String>) {
    match prev {
        Some(v) => env::set_var(key, v),
        None => env::remove_var(key),
    }
}

fn make_resolved(
    provider: &str,
    model: &str,
    protocol: ApiProtocol,
    base_url: &str,
) -> ResolvedModel {
    ResolvedModel {
        canonical_id: model.to_string(),
        provider: provider.to_string(),
        api_key: Some("sk-test-e2e".to_string()),
        base_url: base_url.to_string(),
        api_protocol: protocol,
        api_model_id: model.to_string(),
        context_length: 131072,
        provider_specific: HashMap::new(),
    }
}

fn user_message(content: &str) -> Message {
    Message {
        role: Role::User,
        content: content.to_string(),
        reasoning_content: None,
        tool_calls: None,
        tool_call_id: None,
        name: None,
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Wave 4: T26 (M3) — Catalog returns Result, no panic
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn catalog_loads_successfully_with_embedded_data() {
    // The catalog is embedded at compile time via include_str!("data.json").
    // A successful load proves the deserialization path works without panicking.
    let catalog =
        artemis_core::catalog::Catalog::get().expect("catalog must load from embedded data.json");
    assert!(
        catalog.model_count() > 50,
        "expected >50 models, got {}",
        catalog.model_count()
    );
}

#[test]
fn catalog_claude_sonnet_entry_exists() {
    let catalog = artemis_core::catalog::Catalog::get().expect("catalog must load");
    let model = catalog
        .get_model("claude-sonnet-4-6")
        .expect("claude-sonnet-4-6 should exist");
    assert!(!model.providers.is_empty());
}

#[test]
fn catalog_resolve_alias_sonnet_works() {
    let catalog = artemis_core::catalog::Catalog::get().expect("catalog must load");
    let resolved = catalog.resolve_alias("sonnet");
    assert!(
        resolved.is_some(),
        "alias 'sonnet' should resolve to a canonical ID"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// Wave 4: T29 (M8) — Error response body truncated
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn error_body_truncated_for_provider_unavailable() {
    // Build a response body exceeding MAX_ERROR_BODY_LENGTH (8192 bytes)
    let large_body = "x".repeat(10_000);
    let err = ErrorClassifier::classify(500, &large_body, "test-provider");

    match err {
        ArtemisError::ProviderUnavailable { reason, .. } => {
            // Reason should be truncated (the body is lowered too).
            assert!(
                reason.len() < 10_000,
                "reason should be truncated, but got {} bytes",
                reason.len()
            );
            assert!(
                reason.ends_with("... (truncated)"),
                "reason should end with truncation marker, got: {}",
                reason
            );
        }
        _ => panic!("Expected ProviderUnavailable, got {err:?}"),
    }
}

#[test]
fn error_body_truncated_for_network_error() {
    let large_body = "y".repeat(9_000);
    // 418 falls through to Network in the catch-all branch
    let err = ErrorClassifier::classify(418, &large_body, "test-provider");

    match err {
        ArtemisError::Network { message, status } => {
            assert_eq!(status, Some(418));
            assert!(
                message.len() < 9_000,
                "message should be truncated, but got {} bytes",
                message.len()
            );
            assert!(
                message.ends_with("... (truncated)"),
                "message should end with truncation marker"
            );
        }
        _ => panic!("Expected Network, got {err:?}"),
    }
}

#[test]
fn error_body_not_truncated_for_small_body() {
    let small_body = "short error message";
    let err = ErrorClassifier::classify(500, small_body, "test-provider");

    match err {
        ArtemisError::ProviderUnavailable { reason, .. } => {
            assert_eq!(reason, small_body.to_lowercase());
            assert!(
                !reason.ends_with("... (truncated)"),
                "small body should not be truncated"
            );
        }
        _ => panic!("Expected ProviderUnavailable, got {err:?}"),
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Wave 4: T30 (M9) — base_url URL format validation
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn validate_base_url_https_ok() {
    assert!(
        router::validate_base_url("https://api.openai.com/v1").is_ok(),
        "valid HTTPS URL should pass"
    );
    assert!(
        router::validate_base_url("https://generativelanguage.googleapis.com/v1beta").is_ok(),
        "HTTPS with long path should pass"
    );
}

#[test]
fn validate_base_url_localhost_ok() {
    assert!(
        router::validate_base_url("http://localhost:8080").is_ok(),
        "localhost should pass"
    );
    assert!(
        router::validate_base_url("http://localhost").is_ok(),
        "localhost without port should pass"
    );
    assert!(
        router::validate_base_url("http://127.0.0.1:11434/v1").is_ok(),
        "127.0.0.1 should pass"
    );
}

#[test]
fn validate_base_url_empty_ok() {
    assert!(
        router::validate_base_url("").is_ok(),
        "empty URL should pass (defaults used)"
    );
}

#[test]
fn validate_base_url_custom_scheme_ok() {
    assert!(
        router::validate_base_url("custom-scheme://host/path").is_ok(),
        "custom scheme with valid host should pass"
    );
}

#[test]
fn validate_base_url_no_host_rejected() {
    assert!(
        router::validate_base_url("https:///path-only").is_err(),
        "URL with no host should be rejected"
    );
    assert!(
        router::validate_base_url("http://").is_err(),
        "URL with no host should be rejected"
    );
}

#[test]
fn validate_base_url_no_scheme_rejected() {
    assert!(
        router::validate_base_url("api.openai.com").is_err(),
        "URL without scheme should be rejected"
    );
    assert!(
        router::validate_base_url("localhost:8080").is_err(),
        "URL without scheme should be rejected"
    );
    assert!(
        router::validate_base_url("not-a-url").is_err(),
        "malformed URL should be rejected"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// Wave 5: T31 (L5) — Anthropic SSE error event handling
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn anthropic_sse_error_event_surfaced() {
    let mut parser = AnthropicSseParser::new();
    let events = parser
        .parse_chunk(
            "error",
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        )
        .unwrap();

    assert_eq!(events.len(), 1, "error event should produce 1 StreamEvent");
    assert_eq!(
        events[0],
        StreamEvent::Error {
            message: "Overloaded".into()
        },
        "error event should be surfaced as StreamEvent::Error"
    );
}

#[test]
fn anthropic_sse_error_event_no_message_field() {
    let mut parser = AnthropicSseParser::new();
    let events = parser
        .parse_chunk(
            "error",
            r#"{"type":"error","error":{"type":"rate_limit_error"}}"#,
        )
        .unwrap();

    // When no "message" field exists, falls back to "type" field
    assert_eq!(events.len(), 1);
    match &events[0] {
        StreamEvent::Error { message } => {
            assert_eq!(message, "rate_limit_error");
        }
        _ => panic!("expected StreamEvent::Error"),
    }
}

#[test]
fn anthropic_sse_error_event_no_type_or_message() {
    let mut parser = AnthropicSseParser::new();
    let events = parser
        .parse_chunk("error", r#"{"type":"error","error":{}}"#)
        .unwrap();

    assert_eq!(events.len(), 1);
    match &events[0] {
        StreamEvent::Error { message } => {
            assert_eq!(message, "Unknown Anthropic streaming error");
        }
        _ => panic!("expected StreamEvent::Error"),
    }
}

#[test]
fn anthropic_sse_ping_event_ignored() {
    let mut parser = AnthropicSseParser::new();
    let events = parser.parse_chunk("ping", r#"{"type":"ping"}"#).unwrap();
    assert!(events.is_empty(), "ping events should be ignored");
}

#[test]
fn anthropic_sse_message_stop_ignored() {
    let mut parser = AnthropicSseParser::new();
    let events = parser
        .parse_chunk("message_stop", r#"{"type":"message_stop"}"#)
        .unwrap();
    assert!(events.is_empty(), "message_stop should be ignored");
}

#[test]
fn anthropic_sse_unknown_event_type_ignored() {
    let mut parser = AnthropicSseParser::new();
    let events = parser
        .parse_chunk("unknown_custom_event", r#"{"type":"custom"}"#)
        .unwrap();
    assert!(
        events.is_empty(),
        "unknown event types should return empty vec (no crash)"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// Wave 5: T32 (L2+L3) — model extraction + retry_after parsing
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn error_classify_404_extracts_model_from_body() {
    let err = ErrorClassifier::classify(
        404,
        r#"{"error":{"message":"not found"},"model":"gpt-4"}"#,
        "openai",
    );
    match err {
        ArtemisError::ModelNotFound { model } => {
            assert_eq!(model, "gpt-4");
        }
        _ => panic!("expected ModelNotFound, got {err:?}"),
    }
}

#[test]
fn error_classify_404_no_model_in_body_falls_back() {
    let err = ErrorClassifier::classify(404, r#"{"error":"not found"}"#, "test");
    match err {
        ArtemisError::ModelNotFound { model } => {
            assert_eq!(model, "unknown", "should fall back to 'unknown'");
        }
        _ => panic!("expected ModelNotFound, got {err:?}"),
    }
}

#[test]
fn error_classify_429_extracts_retry_after_numeric() {
    let err = ErrorClassifier::classify(
        429,
        r#"{"error":"rate limit","retry_after":45}"#,
        "provider",
    );
    match err {
        ArtemisError::RateLimit { retry_after, .. } => {
            assert_eq!(retry_after, Some(45.0));
        }
        _ => panic!("expected RateLimit, got {err:?}"),
    }
}

#[test]
fn error_classify_429_extracts_retry_after_float() {
    let err = ErrorClassifier::classify(429, r#"{"retry_after":3.5}"#, "provider");
    match err {
        ArtemisError::RateLimit { retry_after, .. } => {
            assert_eq!(retry_after, Some(3.5));
        }
        _ => panic!("expected RateLimit, got {err:?}"),
    }
}

#[test]
fn error_classify_429_extracts_retry_after_hyphenated_key() {
    let err = ErrorClassifier::classify(429, r#"{"retry-after":20}"#, "provider");
    match err {
        ArtemisError::RateLimit { retry_after, .. } => {
            assert_eq!(retry_after, Some(20.0));
        }
        _ => panic!("expected RateLimit, got {err:?}"),
    }
}

#[test]
fn error_classify_429_no_retry_after_returns_none() {
    let err = ErrorClassifier::classify(429, r#"{"error":"rate limited"}"#, "provider");
    match err {
        ArtemisError::RateLimit { retry_after, .. } => {
            assert_eq!(retry_after, None);
        }
        _ => panic!("expected RateLimit, got {err:?}"),
    }
}

#[test]
fn error_classify_429_retry_after_from_non_json_body() {
    // Body is not JSON, but the extractor still scans for `retry_after:` pattern
    let err = ErrorClassifier::classify(429, "retry_after: 25 seconds", "provider");
    match err {
        ArtemisError::RateLimit { retry_after, .. } => {
            assert_eq!(retry_after, Some(25.0));
        }
        _ => panic!("expected RateLimit, got {err:?}"),
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Wave 5: T33 (L4) — HTTP status code classification matrix
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn error_classify_status_401_authentication() {
    let err = ErrorClassifier::classify(401, "Unauthorized", "provider");
    assert!(matches!(err, ArtemisError::Authentication { .. }));
}

#[test]
fn error_classify_status_403_authentication() {
    let err = ErrorClassifier::classify(403, "Forbidden", "provider");
    assert!(matches!(err, ArtemisError::Authentication { .. }));
}

#[test]
fn error_classify_status_404_model_not_found() {
    let err = ErrorClassifier::classify(404, r#"{"model":"unknown-model"}"#, "provider");
    assert!(matches!(err, ArtemisError::ModelNotFound { .. }));
}

#[test]
fn error_classify_status_429_rate_limit() {
    let err = ErrorClassifier::classify(429, "{}", "provider");
    assert!(matches!(err, ArtemisError::RateLimit { .. }));
}

#[test]
fn error_classify_status_500_provider_unavailable() {
    let err = ErrorClassifier::classify(500, "internal error", "provider");
    assert!(matches!(err, ArtemisError::ProviderUnavailable { .. }));
}

#[test]
fn error_classify_status_502_provider_unavailable() {
    let err = ErrorClassifier::classify(502, "bad gateway", "provider");
    assert!(matches!(err, ArtemisError::ProviderUnavailable { .. }));
}

#[test]
fn error_classify_status_503_provider_unavailable() {
    let err = ErrorClassifier::classify(503, "service unavailable", "provider");
    assert!(matches!(err, ArtemisError::ProviderUnavailable { .. }));
}

#[test]
fn error_classify_status_400_context_window_exceeded() {
    let err = ErrorClassifier::classify(
        400,
        r#"{"error":{"code":"context_length_exceeded"}}"#,
        "provider",
    );
    assert!(
        matches!(err, ArtemisError::ContextWindowExceeded { .. }),
        "400 with context_length_exceeded body should be ContextWindowExceeded"
    );
}

#[test]
fn error_classify_status_400_without_context_overflow_is_network() {
    let err = ErrorClassifier::classify(400, "bad request", "provider");
    assert!(
        matches!(
            err,
            ArtemisError::Network {
                status: Some(400),
                ..
            }
        ),
        "400 without context overflow should be Network"
    );
}

#[test]
fn error_classify_status_408_is_retryable() {
    // FIXED (M4): 408 is now classified as ProviderUnavailable (retryable).
    let err = ErrorClassifier::classify(408, "Request Timeout", "provider");
    match err {
        ArtemisError::ProviderUnavailable { provider, .. } => {
            assert_eq!(provider, "provider");
        }
        _ => panic!("expected ProviderUnavailable for 408, got {err:?}"),
    }
}

#[test]
fn error_classify_status_0_is_network() {
    let err = ErrorClassifier::classify(0, "connection refused", "provider");
    assert!(
        matches!(
            err,
            ArtemisError::Network {
                status: Some(0),
                ..
            }
        ),
        "status 0 (no response) should be Network"
    );
}

#[test]
fn error_classify_unhandled_status_is_network() {
    let err = ErrorClassifier::classify(418, "I'm a teapot", "provider");
    assert!(
        matches!(
            err,
            ArtemisError::Network {
                status: Some(418),
                ..
            }
        ),
        "unhandled status should be Network"
    );
}

#[test]
fn error_classify_is_retryable_rate_limit() {
    let err = ArtemisError::RateLimit {
        retry_after: None,
        provider: "test".into(),
    };
    assert!(
        artemis_core::errors::ErrorClassifier::is_retryable(&err),
        "RateLimit should be retryable"
    );
}

#[test]
fn error_classify_is_retryable_provider_unavailable() {
    let err = ArtemisError::ProviderUnavailable {
        provider: "test".into(),
        reason: "overloaded".into(),
    };
    assert!(
        artemis_core::errors::ErrorClassifier::is_retryable(&err),
        "ProviderUnavailable should be retryable"
    );
}

#[test]
fn error_classify_is_not_retryable_for_other_errors() {
    assert!(!ErrorClassifier::is_retryable(
        &ArtemisError::Authentication {
            provider: "test".into()
        }
    ));
    assert!(!ErrorClassifier::is_retryable(
        &ArtemisError::ModelNotFound {
            model: "test".into()
        }
    ));
    assert!(!ErrorClassifier::is_retryable(
        &ArtemisError::ContextWindowExceeded {
            tokens: 1000,
            limit: 1000
        }
    ));
    assert!(!ErrorClassifier::is_retryable(&ArtemisError::Network {
        message: "timeout".into(),
        status: Some(504)
    }));
    assert!(!ErrorClassifier::is_retryable(&ArtemisError::Config {
        message: "missing key".into()
    }));
}

// ════════════════════════════════════════════════════════════════════════════
// Wave 5: T34 (L7) — resolve_permissive with uppercase/mixed-case providers
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn resolve_permissive_works_with_lowercase_provider() {
    let router = ModelRouter::new();
    let result = router.resolve_permissive("anthropic/claude-sonnet-4.6");
    assert!(
        result.is_ok(),
        "should resolve known provider in provider/model format"
    );
    let model = result.unwrap();
    assert_eq!(model.provider, "anthropic");
    assert_eq!(model.api_protocol, ApiProtocol::AnthropicMessages);
}

#[test]
fn resolve_permissive_works_with_uppercase_provider() {
    // L7 regression: verify provider is lowercased during permissive resolution
    let router = ModelRouter::new();
    let result = router.resolve_permissive("ANTHROPIC/claude-sonnet-4.6");
    assert!(
        result.is_ok(),
        "uppercase provider should be normalized to lowercase"
    );
    let model = result.unwrap();
    assert_eq!(model.provider, "anthropic");
}

#[test]
fn resolve_permissive_works_with_mixed_case_provider() {
    let router = ModelRouter::new();
    let result = router.resolve_permissive("OpenAI/gpt-4o");
    assert!(
        result.is_ok(),
        "mixed-case provider should be normalized to lowercase"
    );
    let model = result.unwrap();
    assert_eq!(model.provider, "openai");
}

#[test]
fn resolve_permissive_rejects_unknown_provider() {
    let router = ModelRouter::new();
    let result = router.resolve_permissive("nonexistent42/model-name");
    assert!(result.is_err(), "unknown provider should return error");
}

// ════════════════════════════════════════════════════════════════════════════
// Wave 5: T35 (L9) — ChatRequest model field consistency
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn chat_request_model_equals_resolved_api_model_id() {
    let resolved = make_resolved(
        "openai",
        "gpt-4o",
        ApiProtocol::OpenAiChat,
        "https://api.openai.com/v1",
    );

    let request = ChatRequest::new(vec![user_message("Hello")], vec![], resolved.clone());

    assert_eq!(
        request.model, request.resolved.api_model_id,
        "ChatRequest.model should always equal resolved.api_model_id"
    );
    assert_eq!(request.model, "gpt-4o");
    assert_eq!(request.resolved.canonical_id, "gpt-4o");
}

#[test]
fn chat_request_preserves_resolved_model_fields() {
    let resolved = make_resolved(
        "groq",
        "mixtral-8x7b",
        ApiProtocol::OpenAiChat,
        "https://api.groq.com/openai/v1",
    );

    let request = ChatRequest::new(vec![user_message("Hi")], vec![], resolved.clone());

    assert_eq!(request.resolved.provider, "groq");
    assert_eq!(request.resolved.base_url, "https://api.groq.com/openai/v1");
    assert_eq!(request.resolved.api_protocol, ApiProtocol::OpenAiChat);
    assert!(request.resolved.api_key.is_some());
    assert_eq!(request.messages.len(), 1);
    assert!(request.tools.is_empty());
    assert!(!request.stream);
}

#[test]
fn chat_request_model_is_distinct_from_canonical_id() {
    // When api_model_id differs from canonical_id (provider-specific naming),
    // ChatRequest.model should use api_model_id, not canonical_id
    let resolved = ResolvedModel {
        canonical_id: "custom-model-v1".to_string(),
        provider: "mock".to_string(),
        api_key: Some("sk-test".to_string()),
        base_url: "https://api.example.com".to_string(),
        api_protocol: ApiProtocol::OpenAiChat,
        api_model_id: "provider-specific-name".to_string(),
        context_length: 131072,
        provider_specific: HashMap::new(),
    };

    let request = ChatRequest::new(vec![user_message("Hi")], vec![], resolved);

    assert_eq!(request.model, "provider-specific-name");
    assert_eq!(request.resolved.canonical_id, "custom-model-v1");
}

// ════════════════════════════════════════════════════════════════════════════
// Wave 5: T31 (L1) — OpenAI SSE delta tracking with orphaned deltas
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn openai_sse_delta_requires_prior_tool_call_start() {
    // Regression: tool call deltas without a prior ToolCallStart should be
    // silently dropped (current behavior — the fix should add a warning).
    let mut parser = OpenAiSseParser::new();

    // Send a delta for index 0 without a prior start event
    let events = parser
        .parse_chunk(
            "message",
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"Args here"}}]}}]}"#,
        )
        .unwrap();

    // Should produce no events — delta silently dropped without matching start
    let tool_deltas: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, StreamEvent::ToolCallDelta { .. }))
        .collect();
    assert!(
        tool_deltas.is_empty(),
        "orphaned delta (no prior ToolCallStart) should be silently dropped"
    );
}

#[test]
fn openai_sse_tool_call_start_then_delta_works() {
    let mut parser = OpenAiSseParser::new();

    // First: ToolCallStart with id
    let events = parser
        .parse_chunk(
            "message",
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"get_weather"}}]}}]}"#,
        )
        .unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, StreamEvent::ToolCallStart { .. })),
        "ToolCallStart should be produced"
    );

    // Second: ToolCallDelta with matching index
    let events = parser
        .parse_chunk(
            "message",
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"city\":"}}]}}]}"#,
        )
        .unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, StreamEvent::ToolCallDelta { .. })),
        "ToolCallDelta with matching start should be produced"
    );
}

#[test]
fn openai_sse_multiple_tool_calls_tracked_correctly() {
    let mut parser = OpenAiSseParser::new();

    // Start tool call 0
    parser
        .parse_chunk(
            "message",
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"tool_a"}}]}}]}"#,
        )
        .unwrap();

    // Start tool call 1 (different index, different id)
    let events = parser
        .parse_chunk(
            "message",
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"call_b","type":"function","function":{"name":"tool_b"}}]}}]}"#,
        )
        .unwrap();
    assert!(
        events
            .iter()
            .any(|e| { matches!(e, StreamEvent::ToolCallStart { id, .. } if id == "call_b") }),
        "second ToolCallStart should have different id"
    );

    // Delta for index 0 should produce call_a's id
    let events = parser
        .parse_chunk(
            "message",
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"a"}}]}}]}"#,
        )
        .unwrap();
    assert!(
        events
            .iter()
            .any(|e| { matches!(e, StreamEvent::ToolCallDelta { id, .. } if id == "call_a") }),
        "delta for index 0 should reference call_a, not call_b"
    );

    // Delta for index 1 should produce call_b's id
    let events = parser
        .parse_chunk(
            "message",
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"function":{"arguments":"b"}}]}}]}"#,
        )
        .unwrap();
    assert!(
        events
            .iter()
            .any(|e| { matches!(e, StreamEvent::ToolCallDelta { id, .. } if id == "call_b") }),
        "delta for index 1 should reference call_b"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// Wave 5: model ID normalization (used by resolve_permissive path)
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn normalize_model_id_lowercases() {
    assert_eq!(router::normalize_model_id("GPT-4O"), "gpt-4o");
    assert_eq!(
        router::normalize_model_id("CLAUDE-SONNET-4-6"),
        "claude-sonnet-4-6"
    );
}

#[test]
fn normalize_model_id_strips_openrouter_prefix() {
    assert_eq!(
        router::normalize_model_id("anthropic/claude-sonnet-4.6"),
        "claude-sonnet-4-6"
    );
    assert_eq!(router::normalize_model_id("openai/gpt-4o"), "gpt-4o");
}

#[test]
fn normalize_model_id_claude_dots_to_hyphens() {
    assert_eq!(
        router::normalize_model_id("claude-sonnet-4.6"),
        "claude-sonnet-4-6"
    );
    assert_eq!(
        router::normalize_model_id("claude-opus-4.7"),
        "claude-opus-4-7"
    );
    assert_eq!(
        router::normalize_model_id("claude-haiku-4.5"),
        "claude-haiku-4-5"
    );
}

#[test]
fn normalize_model_id_noop_for_non_claude() {
    assert_eq!(router::normalize_model_id("gpt-4o"), "gpt-4o");
    assert_eq!(
        router::normalize_model_id("deepseek-v4-pro"),
        "deepseek-v4-pro"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// Wave 4+5: Router resolve full pipeline with env var setup
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn router_resolve_uses_correct_provider_details() {
    let _lock = crate::env_lock::lock();
    let saved_keys: Vec<(String, Option<String>)> = [
        "ANTHROPIC_API_KEY",
        "NOUS_API_KEY",
        "GITHUB_TOKEN",
        "OPENCODE_ZEN_API_KEY",
        "KILO_API_KEY",
        "AI_GATEWAY_API_KEY",
        "OPENAI_API_KEY",
    ]
    .iter()
    .map(|k| (k.to_string(), save_env(k)))
    .collect();

    // Clear all competing credentials, set only Anthropic
    for k in &[
        "NOUS_API_KEY",
        "GITHUB_TOKEN",
        "OPENCODE_ZEN_API_KEY",
        "KILO_API_KEY",
        "AI_GATEWAY_API_KEY",
        "OPENAI_API_KEY",
    ] {
        env::remove_var(k);
    }
    env::set_var("ANTHROPIC_API_KEY", "sk-ant-test-wave45");

    let router = ModelRouter::new();
    let resolved = router
        .resolve("sonnet", None)
        .expect("sonnet should resolve via Anthropic");

    assert_eq!(resolved.canonical_id, "claude-sonnet-4-6");
    assert_eq!(resolved.provider, "anthropic");
    assert_eq!(resolved.api_protocol, ApiProtocol::AnthropicMessages);
    assert_eq!(resolved.api_key.as_deref(), Some("sk-ant-test-wave45"));

    for (k, v) in saved_keys {
        restore_env(&k, v);
    }
}

#[test]
fn router_resolve_permissive_gives_api_model_id_not_canonical_id() {
    let router = ModelRouter::new();
    let result = router.resolve_permissive("openai/gpt-4o");
    assert!(result.is_ok());
    let model = result.unwrap();
    // api_model_id should be the model part from the input
    assert_eq!(model.api_model_id, "gpt-4o");
    assert_eq!(model.provider, "openai");
}

// ════════════════════════════════════════════════════════════════════════════
// Wave 5: Provider defaults used by resolve_permissive
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn resolve_permissive_uses_provider_defaults_for_base_url() {
    let router = ModelRouter::new();
    let result = router.resolve_permissive("anthropic/claude-sonnet-4-6");
    assert!(result.is_ok());
    let model = result.unwrap();
    assert_eq!(model.base_url, "https://api.anthropic.com");
    assert_eq!(model.api_protocol, ApiProtocol::AnthropicMessages);
}

#[test]
fn resolve_permissive_uses_provider_defaults_for_gemini() {
    let router = ModelRouter::new();
    let result = router.resolve_permissive("gemini/gemini-2.5-flash");
    assert!(result.is_ok());
    let model = result.unwrap();
    assert_eq!(model.provider, "gemini");
    assert_eq!(model.api_protocol, ApiProtocol::GeminiGenerateContent);
}

// ════════════════════════════════════════════════════════════════════════════
// Wave 5: ToolDefinition serialization roundtrip
// ════════════════════════════════════════════════════════════════════════════

#[test]
fn tool_definition_serialization_roundtrip() {
    let td = ToolDefinition {
        name: "get_weather".to_string(),
        description: "Get current weather".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "city": {"type": "string"}
            },
            "required": ["city"]
        }),
    };

    // Verify the parameters JSON is preserved
    let params_str = serde_json::to_string(&td.parameters).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&params_str).unwrap();
    assert_eq!(parsed["type"], "object");
    assert_eq!(parsed["properties"]["city"]["type"], "string");
}

#[test]
fn tool_definition_parameters_must_be_object() {
    // While the Rust struct doesn't enforce this, the PyO3 #[new] does.
    // This test verifies that the struct is constructible in Rust with any JSON.
    let td = ToolDefinition {
        name: "test".to_string(),
        description: "test".to_string(),
        parameters: json!({"valid": "object"}),
    };
    assert!(td.parameters.is_object());
}
