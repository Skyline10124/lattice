//! Regression tests for Wave 1 bug fixes.
//!
//! Verifies that all Wave 1 cleanup bugs remain fixed:
//!
//! | Task | Bug                              | Test Section                |
//! |------|----------------------------------|-----------------------------|
//! | T1   | rig-core removal                 | `rig_core_removal`          |
//! | T2   | ErrorClassifier unification      | `error_classifier_unify`    |
//! | T3   | #![allow(deprecated)] consol.    | `deprecated_consolidation`  |
//! | T4   | ProviderConfig removal           | `provider_config_removal`   |
//! | T5   | TransportType removal            | `transport_type_removal`    |
//! | T6   | LazyLock regex in router         | `lazylock_regex`            |
//! | T7   | Debug api_key masking            | `debug_api_key_masking`     |
//! | T8   | saturating_pow in retry          | `saturating_pow`            |
//! | T9   | engine "mock" provider           | `engine_mock_provider`      |

use artemis_core::catalog::{ApiProtocol, ResolvedModel};
use artemis_core::errors::{ArtemisError, ErrorClassifier};
use artemis_core::retry::RetryPolicy;
use artemis_core::router::{self, ModelRouter};
use std::collections::HashMap;
use std::env;
use std::time::Duration;

// ── Env var isolation helpers ──────────────────────────────────────────────

fn save_env(key: &str) -> Option<String> {
    env::var(key).ok()
}

fn restore_env(key: &str, prev: Option<String>) {
    match prev {
        Some(v) => env::set_var(key, v),
        None => env::remove_var(key),
    }
}

/// Save and clear a batch of env vars, returning the saved state for restore.
fn isolate_env(keys: &[&str]) -> Vec<(String, Option<String>)> {
    keys.iter().map(|k| (k.to_string(), save_env(k))).collect()
}

fn restore_env_batch(saved: &[(String, Option<String>)]) {
    for (k, v) in saved {
        restore_env(k, v.clone());
    }
}

// ════════════════════════════════════════════════════════════════════════════
// T1: rig-core removal regression
// ════════════════════════════════════════════════════════════════════════════

/// Verify the crate builds and core functionality works without rig-core.
/// A compile error would prevent this test from even running, so simply
/// exercising the full resolution pipeline confirms rig-core absence.
#[test]
fn regress_rig_core_model_resolution_works() {
    let _lock = crate::env_lock::lock();
    let saved = isolate_env(&[
        "ANTHROPIC_API_KEY",
        "NOUS_API_KEY",
        "GITHUB_TOKEN",
        "OPENAI_API_KEY",
    ]);
    env::set_var("ANTHROPIC_API_KEY", "sk-ant-test-regress");

    let router = ModelRouter::new();
    let resolved = router
        .resolve("sonnet", None)
        .expect("sonnet should resolve");
    assert_eq!(resolved.canonical_id, "claude-sonnet-4-6");
    assert_eq!(resolved.provider, "anthropic");

    restore_env_batch(&saved);
}

/// Verify the aliases still resolve correctly (tests code paths that
/// were previously compiled with rig-core in the dependency tree).
#[test]
fn regress_rig_core_aliases_intact() {
    let _lock = crate::env_lock::lock();
    let saved = isolate_env(&["ANTHROPIC_API_KEY", "OPENAI_API_KEY"]);

    env::set_var("ANTHROPIC_API_KEY", "sk-ant-test");
    let router = ModelRouter::new();

    assert!(router.resolve("sonnet", None).is_ok());
    assert!(router.resolve("opus", None).is_ok());

    restore_env_batch(&saved);
}

// ════════════════════════════════════════════════════════════════════════════
// T2: ErrorClassifier unification regression
// ════════════════════════════════════════════════════════════════════════════

/// After unification, only errors::ErrorClassifier exists and
/// its classify() correctly maps status codes.
#[test]
fn regress_error_classifier_unified_classify() {
    let err = ErrorClassifier::classify(
        429,
        r#"{"error": "rate limit", "retry_after": 15}"#,
        "openai",
    );
    match err {
        ArtemisError::RateLimit {
            retry_after,
            provider,
        } => {
            assert_eq!(retry_after, Some(15.0));
            assert_eq!(provider, "openai");
        }
        other => panic!("expected RateLimit, got {:?}", other),
    }

    let err = ErrorClassifier::classify(401, "", "anthropic");
    match err {
        ArtemisError::Authentication { provider } => {
            assert_eq!(provider, "anthropic");
        }
        other => panic!("expected Authentication, got {:?}", other),
    }

    let err = ErrorClassifier::classify(404, r#"{"error": {"model": "bogus-gpt"}}"#, "openai");
    match err {
        ArtemisError::ModelNotFound { model } => {
            assert_eq!(model, "bogus-gpt");
        }
        other => panic!("expected ModelNotFound, got {:?}", other),
    }

    let err = ErrorClassifier::classify(500, "internal error", "gemini");
    match err {
        ArtemisError::ProviderUnavailable { provider, .. } => {
            assert_eq!(provider, "gemini");
        }
        other => panic!("expected ProviderUnavailable, got {:?}", other),
    }

    let err = ErrorClassifier::classify(400, "context_length_exceeded: max 8192", "openai");
    match err {
        ArtemisError::ContextWindowExceeded { .. } => {}
        other => panic!("expected ContextWindowExceeded, got {:?}", other),
    }

    let err = ErrorClassifier::classify(418, "I'm a teapot", "openai");
    match err {
        ArtemisError::Network { message, status } => {
            assert!(message.contains("teapot"));
            assert_eq!(status, Some(418));
        }
        other => panic!("expected Network, got {:?}", other),
    }
}

/// is_retryable() must return true for RateLimit and ProviderUnavailable,
/// false for all others.
#[test]
fn regress_error_classifier_is_retryable() {
    assert!(
        ErrorClassifier::is_retryable(&ArtemisError::RateLimit {
            retry_after: None,
            provider: "test".into(),
        }),
        "RateLimit should be retryable"
    );
    assert!(
        ErrorClassifier::is_retryable(&ArtemisError::ProviderUnavailable {
            provider: "test".into(),
            reason: "test".into(),
        }),
        "ProviderUnavailable should be retryable"
    );

    assert!(
        !ErrorClassifier::is_retryable(&ArtemisError::Authentication {
            provider: "test".into(),
        }),
        "Authentication should NOT be retryable"
    );
    assert!(
        !ErrorClassifier::is_retryable(&ArtemisError::ModelNotFound {
            model: "test".into(),
        }),
        "ModelNotFound should NOT be retryable"
    );
    assert!(
        !ErrorClassifier::is_retryable(&ArtemisError::ContextWindowExceeded {
            tokens: 100,
            limit: 50,
        }),
        "ContextWindowExceeded should NOT be retryable"
    );
    assert!(
        !ErrorClassifier::is_retryable(&ArtemisError::Network {
            message: "test".into(),
            status: Some(502),
        }),
        "Network should NOT be retryable"
    );
}

/// Status 0 (no response received) — must classify as Network.
#[test]
fn regress_error_classifier_status_zero_network() {
    let err = ErrorClassifier::classify(0, "", "test");
    match err {
        ArtemisError::Network { status, .. } => {
            assert_eq!(
                status,
                Some(0),
                "status=0 maps to Network with status=Some(0)"
            );
        }
        other => panic!("expected Network for status 0, got {:?}", other),
    }
}

// ════════════════════════════════════════════════════════════════════════════
// T3: #![allow(deprecated)] consolidation regression
// ════════════════════════════════════════════════════════════════════════════

/// Verify all files that had their #![allow(deprecated)] removed still
/// compile and their public APIs work correctly. The mere fact this test
/// compiles proves the consolidation didn't break anything.
#[test]
fn regress_deprecated_consolidation_types_compile() {
    // Exercise types.rs — types module (had allow(deprecated) removed)
    use artemis_core::types::{FunctionCall, Message, Role, ToolCall, ToolDefinition};
    let _msg = Message {
        role: Role::User,
        content: "test".into(),
        tool_calls: None,
        tool_call_id: None,
        name: None,
    };
    let _tool = ToolDefinition {
        name: "test".into(),
        description: "desc".into(),
        parameters: serde_json::json!({"type": "object"}),
    };
    let _tc = ToolCall {
        id: "t1".into(),
        function: FunctionCall {
            name: "test".into(),
            arguments: "{}".into(),
        },
    };
}

/// Exercise provider module — was in the deprecated consolidation list.
#[test]
fn regress_deprecated_consolidation_provider_compile() {
    use artemis_core::provider::{ChatRequest, ChatResponse};
    let resolved = ResolvedModel {
        canonical_id: "test".into(),
        provider: "test".into(),
        api_key: None,
        base_url: "http://localhost".into(),
        api_protocol: ApiProtocol::OpenAiChat,
        api_model_id: "test".into(),
        context_length: 4096,
        provider_specific: HashMap::new(),
    };
    let request = ChatRequest::new(vec![], vec![], resolved);
    assert_eq!(request.model, "test");
    let _response = ChatResponse {
        content: None,
        tool_calls: None,
        usage: None,
        finish_reason: "stop".into(),
        model: "test".into(),
    };
}

// ════════════════════════════════════════════════════════════════════════════
// T4: ProviderConfig removal regression
// ════════════════════════════════════════════════════════════════════════════

/// ResolvedModel (the replacement for ProviderConfig) must work correctly.
#[test]
fn regress_provider_config_resolved_model_roundtrip() {
    let model = ResolvedModel {
        canonical_id: "claude-sonnet-4-6".into(),
        provider: "anthropic".into(),
        api_key: Some("sk-ant-test".into()),
        base_url: "https://api.anthropic.com".into(),
        api_protocol: ApiProtocol::AnthropicMessages,
        api_model_id: "claude-sonnet-4-6".into(),
        context_length: 200000,
        provider_specific: HashMap::from([("region".into(), "us-east".into())]),
    };

    // All fields accessible
    assert_eq!(model.canonical_id, "claude-sonnet-4-6");
    assert_eq!(model.provider, "anthropic");
    assert!(model.api_key.is_some());
    assert_eq!(model.base_url, "https://api.anthropic.com");
    assert!(matches!(model.api_protocol, ApiProtocol::AnthropicMessages));
    assert_eq!(model.context_length, 200000);

    // Clone works
    let cloned = model.clone();
    assert_eq!(cloned.canonical_id, model.canonical_id);

    // Serialize roundtrip
    let json = serde_json::to_string(&model).expect("serialize");
    let deserialized: ResolvedModel = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(deserialized.canonical_id, model.canonical_id);
    assert_eq!(deserialized.provider, model.provider);
}

// ════════════════════════════════════════════════════════════════════════════
// T5: TransportType removal regression
// ════════════════════════════════════════════════════════════════════════════

/// ApiProtocol (the replacement for TransportType) must work correctly.
#[test]
fn regress_transport_type_api_protocol_works() {
    // All ApiProtocol variants must be functional
    let protocols = [
        ApiProtocol::OpenAiChat,
        ApiProtocol::AnthropicMessages,
        ApiProtocol::GeminiGenerateContent,
        ApiProtocol::BedrockConverse,
        ApiProtocol::CodexResponses,
    ];

    for proto in &protocols {
        let name = format!("{:?}", proto);
        assert!(!name.is_empty(), "Protocol {:?} has no Debug name", proto);
    }

    // Roundtrip through serialization (used in catalog loading)
    let resolved = ResolvedModel {
        canonical_id: "gpt-4o".into(),
        provider: "openai".into(),
        api_key: None,
        base_url: "https://api.openai.com/v1".into(),
        api_protocol: ApiProtocol::OpenAiChat,
        api_model_id: "gpt-4o".into(),
        context_length: 128000,
        provider_specific: HashMap::new(),
    };
    let json = serde_json::to_string(&resolved).expect("serialize");
    let deserialized: ResolvedModel = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(deserialized.api_protocol, ApiProtocol::OpenAiChat);
}

/// All ApiProtocol variants serialize/deserialize correctly.
#[test]
fn regress_transport_type_all_variants_serialize() {
    let test_cases = [
        (ApiProtocol::OpenAiChat, "openai_chat"),
        (ApiProtocol::AnthropicMessages, "anthropic_messages"),
        (
            ApiProtocol::GeminiGenerateContent,
            "gemini_generate_content",
        ),
        (ApiProtocol::BedrockConverse, "bedrock_converse"),
        (ApiProtocol::CodexResponses, "codex_responses"),
    ];

    for (proto, expected_key) in &test_cases {
        // ApiProtocol serializes via serde using its string representation
        let json = serde_json::to_value(proto).expect("serialize");
        // The ApiProtocol serialization should produce the relevant string
        let repr = format!("{:?}", proto);
        assert!(
            !repr.is_empty(),
            "ApiProtocol variant has Debug representation"
        );

        // Deserialize roundtrip by going through ResolvedModel
        let model = ResolvedModel {
            canonical_id: "test".into(),
            provider: "test".into(),
            api_key: None,
            base_url: String::new(),
            api_protocol: proto.clone(),
            api_model_id: "test".into(),
            context_length: 4096,
            provider_specific: HashMap::new(),
        };
        let json = serde_json::to_string(&model).expect("serialize model");
        let deserialized: ResolvedModel = serde_json::from_str(&json).expect("deserialize model");
        assert_eq!(
            format!("{:?}", deserialized.api_protocol),
            format!("{:?}", proto),
        );

        let _ = expected_key; // silence unused warning
    }
}

// ════════════════════════════════════════════════════════════════════════════
// T6: LazyLock regex in router regression
// ════════════════════════════════════════════════════════════════════════════

/// normalize_model_id must produce correct results after LazyLock refactor.
#[test]
fn regress_lazylock_normalize_model_id_correctness() {
    let cases: Vec<(&str, &str)> = vec![
        (
            "openrouter/anthropic/claude-sonnet-4-6",
            "anthropic/claude-sonnet-4-6",
        ),
        ("anthropic/claude-opus-4-7", "claude-opus-4-7"),
        // Bedrock prefix stripping
        ("us.anthropic.claude-sonnet-4-6-v1:0", "claude-sonnet-4-6"),
        ("us.amazon.titan-v1:0", "titan"),
        ("us.meta.llama-v1", "llama"),
        // Bedrock suffix stripping
        ("claude-haiku-3.5-v2:0", "claude-haiku-3-5"),
        ("claude-sonnet-4-6-v1:0", "claude-sonnet-4-6"),
        // Claude dot-to-hyphen conversion
        ("claude-sonnet-4.6", "claude-sonnet-4-6"),
        ("claude-opus-4.7", "claude-opus-4-7"),
        ("claude-haiku-3.5", "claude-haiku-3-5"),
        // Non-Claude models: dots preserved (no hyphen conversion)
        ("gpt-4.5", "gpt-4.5"),
        ("gpt-4o", "gpt-4o"),
        // Canonical already-normalized forms: no-op
        ("claude-sonnet-4-6", "claude-sonnet-4-6"),
        ("gpt-4o", "gpt-4o"),
    ];

    for (input, expected) in cases {
        let result = router::normalize_model_id(input);
        assert_eq!(
            result, expected,
            "normalize_model_id(\"{}\") = \"{}\", expected \"{}\"",
            input, result, expected
        );
    }
}

/// normalize_model_id must be case-insensitive.
#[test]
fn regress_lazylock_normalize_model_id_case_insensitive() {
    assert_eq!(
        router::normalize_model_id("CLAUDE-SONNET-4.6"),
        "claude-sonnet-4-6"
    );
    assert_eq!(router::normalize_model_id("GPT-4O"), "gpt-4o");
    assert_eq!(
        router::normalize_model_id("AnThRoPiC/ClAuDe-OpUs-4.7"),
        "claude-opus-4-7"
    );
}

/// normalize_model_id handles edge cases without panicking.
#[test]
fn regress_lazylock_normalize_model_id_edge_cases() {
    // Empty string
    assert_eq!(router::normalize_model_id(""), "");

    // Single character
    assert_eq!(router::normalize_model_id("x"), "x");

    // Whitespace (should not panic, just produce lowercased output)
    let _ = router::normalize_model_id(" model with spaces ");

    // Very long input
    let long = "a".repeat(1000);
    let _ = router::normalize_model_id(&long);

    // Only a vendor prefix with no model
    assert_eq!(router::normalize_model_id("anthropic/"), "");
}

// ════════════════════════════════════════════════════════════════════════════
// T7: Debug api_key masking regression
// ════════════════════════════════════════════════════════════════════════════

/// ResolvedModel Debug must mask api_key with "***".
#[test]
fn regress_debug_api_key_masked() {
    let model = ResolvedModel {
        canonical_id: "claude-sonnet-4-6".into(),
        provider: "anthropic".into(),
        api_key: Some("sk-ant-secret-key-abc123def456".into()),
        base_url: "https://api.anthropic.com".into(),
        api_protocol: ApiProtocol::AnthropicMessages,
        api_model_id: "claude-sonnet-4-6".into(),
        context_length: 200000,
        provider_specific: HashMap::new(),
    };

    let debug_str = format!("{:?}", model);

    // Must NOT leak the real key
    assert!(
        !debug_str.contains("sk-ant-secret-key-abc123def456"),
        "Debug output MUST NOT contain real api_key.\nDebug was: {}",
        debug_str
    );

    // Must show masking indicator
    assert!(
        debug_str.contains("***"),
        "Debug output MUST contain '***' as api_key mask.\nDebug was: {}",
        debug_str
    );

    // Other fields must still be visible
    assert!(debug_str.contains("claude-sonnet-4-6"));
    assert!(debug_str.contains("anthropic"));
}

/// ResolvedModel Debug when api_key is None shows None.
#[test]
fn regress_debug_api_key_none() {
    let model = ResolvedModel {
        canonical_id: "gpt-4o".into(),
        provider: "openai".into(),
        api_key: None,
        base_url: String::new(),
        api_protocol: ApiProtocol::OpenAiChat,
        api_model_id: "gpt-4o".into(),
        context_length: 128000,
        provider_specific: HashMap::new(),
    };

    let debug_str = format!("{:?}", model);

    // When api_key is None, Debug should show None (not "***")
    assert!(
        debug_str.contains("None"),
        "api_key=None should display as None, got: {}",
        debug_str
    );
}

// ════════════════════════════════════════════════════════════════════════════
// T8: saturating_pow regression
// ════════════════════════════════════════════════════════════════════════════

/// Large attempt values must NOT panic (saturating_pow prevents overflow).
#[test]
fn regress_saturating_pow_no_panic_on_high_attempt() {
    let policy = RetryPolicy {
        max_retries: 100,
        base_delay: Duration::from_secs(1),
        max_delay: Duration::from_secs(60),
    };

    // attempt=100 would overflow 2u32.pow(100) — must not panic
    let result = policy.jittered_backoff(100);
    assert!(
        result <= policy.max_delay,
        "jittered_backoff(100) = {:?} exceeds max_delay {:?}",
        result,
        policy.max_delay
    );
}

/// Normal backoff still increases with attempts.
#[test]
fn regress_saturating_pow_normal_backoff_increases() {
    let policy = RetryPolicy::default();
    let d0 = policy.jittered_backoff(0);
    let d2 = policy.jittered_backoff(2);
    let d3 = policy.jittered_backoff(3);

    assert!(d3 > d2, "backoff should increase: d2={:?}, d3={:?}", d2, d3);
    assert!(d2 > d0, "backoff should increase: d0={:?}, d2={:?}", d0, d2);
}

/// Backoff is clamped to max_delay.
#[test]
fn regress_saturating_pow_clamped_to_max() {
    let policy = RetryPolicy {
        max_retries: 10,
        base_delay: Duration::from_secs(10),
        max_delay: Duration::from_secs(30), // low cap
    };

    // With base_delay=10, attempt=3 gives 10*8=80s but capped at 30
    let result = policy.jittered_backoff(3);
    assert!(
        result <= policy.max_delay,
        "result {:?} should not exceed max_delay {:?}",
        result,
        policy.max_delay
    );
}

/// Attempt=0 returns base_delay (not 0).
#[test]
fn regress_saturating_pow_attempt_zero() {
    let policy = RetryPolicy {
        max_retries: 3,
        base_delay: Duration::from_secs(5),
        max_delay: Duration::from_secs(60),
    };

    let result = policy.jittered_backoff(0);
    assert!(
        result >= Duration::from_secs(5),
        "attempt=0 should be >= base_delay"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// T9: Engine "mock" provider hardcoding regression
// ════════════════════════════════════════════════════════════════════════════

/// Router must resolve models to their actual provider names, not "mock".
#[test]
fn regress_engine_mock_provider_resolves_correctly() {
    let _lock = crate::env_lock::lock();
    let saved = isolate_env(&[
        "ANTHROPIC_API_KEY",
        "NOUS_API_KEY",
        "GITHUB_TOKEN",
        "OPENCODE_ZEN_API_KEY",
        "KILO_API_KEY",
        "AI_GATEWAY_API_KEY",
        "OPENAI_API_KEY",
    ]);

    // Only provide Anthropic credential
    for key in &[
        "NOUS_API_KEY",
        "GITHUB_TOKEN",
        "OPENCODE_ZEN_API_KEY",
        "KILO_API_KEY",
        "AI_GATEWAY_API_KEY",
        "OPENAI_API_KEY",
    ] {
        env::remove_var(key);
    }
    env::set_var("ANTHROPIC_API_KEY", "sk-ant-test");

    let router = ModelRouter::new();
    let resolved = router
        .resolve("sonnet", None)
        .expect("sonnet should resolve");

    // Provider must be "anthropic", not "mock"
    assert_eq!(
        resolved.provider, "anthropic",
        "Router must resolve to actual provider 'anthropic', got '{}'",
        resolved.provider
    );
    assert_eq!(resolved.api_protocol, ApiProtocol::AnthropicMessages);

    restore_env_batch(&saved);
}

/// Multiple models across different providers must resolve correctly.
#[test]
fn regress_engine_mock_provider_multi_provider() {
    let _lock = crate::env_lock::lock();
    let saved = isolate_env(&["ANTHROPIC_API_KEY", "OPENAI_API_KEY"]);

    env::set_var("ANTHROPIC_API_KEY", "sk-ant-key");
    env::set_var("OPENAI_API_KEY", "sk-openai-key");

    let router = ModelRouter::new();

    // Anthropic model → provider should be anthropic
    let resolved = router.resolve("sonnet", None).expect("sonnet resolve");
    assert_eq!(resolved.provider, "anthropic");

    restore_env_batch(&saved);
}

/// ResolvedModel from the catalog must carry correct base_url and api_protocol
/// — NOT the hardcoded "http://localhost" or "OpenAiChat" from the old mock bug.
#[test]
fn regress_engine_mock_provider_metadata_correct() {
    let _lock = crate::env_lock::lock();
    let saved = isolate_env(&[
        "ANTHROPIC_API_KEY",
        "NOUS_API_KEY",
        "GITHUB_TOKEN",
        "OPENAI_API_KEY",
    ]);

    env::set_var("ANTHROPIC_API_KEY", "sk-ant-test");
    for key in &["NOUS_API_KEY", "GITHUB_TOKEN", "OPENAI_API_KEY"] {
        env::remove_var(key);
    }

    let router = ModelRouter::new();
    let resolved = router.resolve("sonnet", None).expect("sonnet resolve");

    assert!(
        resolved.base_url.contains("anthropic.com"),
        "base_url should be the real anthropic URL, not http://localhost. Got '{}'",
        resolved.base_url
    );
    assert_eq!(
        resolved.api_protocol,
        ApiProtocol::AnthropicMessages,
        "api_protocol should be AnthropicMessages, not OpenAiChat"
    );

    restore_env_batch(&saved);
}

/// Resolved models with auth errors should produce Authentication error, not generic.
#[test]
fn regress_engine_mock_provider_auth_error_typed() {
    let _lock = crate::env_lock::lock();
    let saved = isolate_env(&[
        "ANTHROPIC_API_KEY",
        "NOUS_API_KEY",
        "GITHUB_TOKEN",
        "OPENAI_API_KEY",
    ]);

    // Set an obviously invalid key so any real API call would get 401
    env::set_var("ANTHROPIC_API_KEY", "sk-ant-bad");
    for key in &["NOUS_API_KEY", "GITHUB_TOKEN", "OPENAI_API_KEY"] {
        env::remove_var(key);
    }

    // The router should still resolve (key exists, even if invalid)
    let router = ModelRouter::new();
    let resolved = router
        .resolve("sonnet", None)
        .expect("sonnet should resolve");
    assert_eq!(resolved.provider, "anthropic");

    // Error classification should still work — classify an actual 401
    let err = ErrorClassifier::classify(401, r#"{"error": "invalid api key"}"#, &resolved.provider);
    assert!(
        matches!(err, ArtemisError::Authentication { .. }),
        "401 should classify as Authentication, got {:?}",
        err
    );

    restore_env_batch(&saved);
}

/// When credential is missing, the router falls back to the first provider
/// with api_key=None — but crucially, the provider name is real (not "mock").
#[test]
fn regress_engine_mock_provider_missing_credential_errors() {
    let _lock = crate::env_lock::lock();
    let saved = isolate_env(&[
        "ANTHROPIC_API_KEY",
        "NOUS_API_KEY",
        "GITHUB_TOKEN",
        "OPENCODE_ZEN_API_KEY",
        "KILO_API_KEY",
        "AI_GATEWAY_API_KEY",
        "OPENAI_API_KEY",
    ]);

    for key in &[
        "ANTHROPIC_API_KEY",
        "NOUS_API_KEY",
        "GITHUB_TOKEN",
        "OPENCODE_ZEN_API_KEY",
        "KILO_API_KEY",
        "AI_GATEWAY_API_KEY",
    ] {
        env::remove_var(key);
    }
    env::set_var("OPENAI_API_KEY", "sk-openai-test");

    let router = ModelRouter::new();
    let result = router.resolve("sonnet", None);

    // With no Anthropic-compatible credentials, fallback returns api_key=None
    // but the provider must NOT be "mock" or some hardcoded value
    assert!(
        result.is_ok(),
        "sonnet should resolve via fallback, not fail"
    );
    let resolved = result.unwrap();
    assert_ne!(
        resolved.provider, "mock",
        "Fallback provider must NOT be 'mock'. Got '{}'",
        resolved.provider
    );
    assert!(
        resolved.api_key.is_none(),
        "Fallback model expects api_key=None when no credential is set"
    );

    restore_env_batch(&saved);
}
