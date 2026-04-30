use lattice_core::catalog::ApiProtocol;
use lattice_core::errors::ArtemisError;
use lattice_core::router::ModelRouter;
use lattice_core::types::{Message, Role};
use std::env;

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

fn isolate_all() -> Vec<(String, Option<String>)> {
    crate::ALL_CREDENTIAL_ENV_VARS
        .iter()
        .map(|k| {
            let key = k.to_string();
            let prev = env::var(&key).ok();
            env::remove_var(&key);
            (key, prev)
        })
        .collect()
}

fn restore_all(saved: &[(String, Option<String>)]) {
    for (key, prev) in saved {
        match prev {
            Some(v) => env::set_var(key, v),
            None => env::remove_var(key),
        }
    }
}

#[test]
fn test_permissive_fallback_provider_model_format() {
    let _lock = crate::env_lock::lock();
    let saved = isolate_all();
    let router = ModelRouter::new();
    let result = router.resolve("anthropic/claude-opus-4", None);
    assert!(
        result.is_ok(),
        "provider/model format should resolve via permissive fallback"
    );

    let resolved = result.unwrap();
    assert_eq!(resolved.provider, "anthropic");
    assert_eq!(resolved.api_protocol, ApiProtocol::AnthropicMessages);
    assert_eq!(resolved.base_url, "https://api.anthropic.com");
    restore_all(&saved);
}

#[test]
fn test_permissive_fallback_openai_model() {
    let _lock = crate::env_lock::lock();
    let saved = isolate_all();
    let router = ModelRouter::new();
    let result = router.resolve_permissive("openai/gpt-4o-mini");
    assert!(
        result.is_ok(),
        "openai/model format should resolve via permissive fallback"
    );

    let resolved = result.unwrap();
    assert_eq!(resolved.provider, "openai");
    assert_eq!(resolved.api_protocol, ApiProtocol::OpenAiChat);
    assert_eq!(resolved.base_url, "https://api.openai.com/v1");
    restore_all(&saved);
}

#[test]
fn test_permissive_fallback_gemini_model() {
    let _lock = crate::env_lock::lock();
    let saved = isolate_all();
    let router = ModelRouter::new();
    let result = router.resolve("gemini/gemini-2.5-flash", None);
    assert!(
        result.is_ok(),
        "gemini/model format should resolve via permissive fallback"
    );

    let resolved = result.unwrap();
    assert_eq!(resolved.provider, "gemini");
    assert_eq!(resolved.api_protocol, ApiProtocol::OpenAiChat);
    restore_all(&saved);
}

#[test]
fn test_permissive_fallback_deepseek_model() {
    let _lock = crate::env_lock::lock();
    let saved = isolate_all();
    // deepseek-chat is in the catalog, so resolve() uses catalog path.
    // Set DEEPSEEK_API_KEY so it resolves via catalog.
    env::set_var("DEEPSEEK_API_KEY", "ds-test-key");
    let router = ModelRouter::new();
    let result = router.resolve("deepseek/deepseek-chat", None);
    assert!(
        result.is_ok(),
        "deepseek/deepseek-chat should resolve via catalog"
    );

    let resolved = result.unwrap();
    assert_eq!(resolved.provider, "deepseek");
    assert_eq!(resolved.api_protocol, ApiProtocol::OpenAiChat);
    restore_all(&saved);
}

#[test]
fn test_unknown_model_no_provider_prefix_fails() {
    let _lock = crate::env_lock::lock();
    let saved = isolate_all();
    let router = ModelRouter::new();
    let result = router.resolve("totally-unknown-model", None);
    assert!(
        result.is_err(),
        "unknown model without provider prefix should fail"
    );

    match result.err().unwrap() {
        ArtemisError::ModelNotFound { model } => {
            assert_eq!(model, "totally-unknown-model");
        }
        other => panic!("Expected ModelNotFound, got {:?}", other),
    }
    restore_all(&saved);
}

#[test]
fn test_unknown_provider_prefix_fails() {
    let _lock = crate::env_lock::lock();
    let saved = isolate_all();
    let router = ModelRouter::new();
    let result = router.resolve("nonexistent-provider/some-model", None);
    assert!(result.is_err(), "unknown provider prefix should fail");

    match result.err().unwrap() {
        ArtemisError::ModelNotFound { .. } => {}
        other => panic!("Expected ModelNotFound, got {:?}", other),
    }
    restore_all(&saved);
}

#[test]
fn test_permissive_resolved_model_fields() {
    let _lock = crate::env_lock::lock();
    let saved = isolate_all();
    let router = ModelRouter::new();
    let resolved = router.resolve_permissive("openai/gpt-4o-mini").unwrap();

    assert_eq!(resolved.provider, "openai");
    assert_eq!(resolved.api_model_id, "gpt-4o-mini");
    assert_eq!(resolved.api_protocol, ApiProtocol::OpenAiChat);
    assert_eq!(resolved.base_url, "https://api.openai.com/v1");
    restore_all(&saved);
}

#[test]
fn test_permissive_resolved_model_usable_in_chat_request() {
    let _lock = crate::env_lock::lock();
    let saved = isolate_all();
    env::set_var("OPENAI_API_KEY", "sk-test");
    use lattice_core::provider::ChatRequest;

    let router = ModelRouter::new();
    let resolved = router.resolve("openai/gpt-4o", None).unwrap();

    let request = ChatRequest::new(vec![user_message("Hello")], vec![], resolved.clone());

    assert!(!request.model.is_empty(), "model field should be populated");
    assert_eq!(request.resolved.canonical_id, "gpt-4o");
    assert_eq!(request.resolved.api_protocol, ApiProtocol::OpenAiChat);
    restore_all(&saved);
}
