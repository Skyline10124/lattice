use artemis_core::catalog::ApiProtocol;
use artemis_core::errors::ArtemisError;
use artemis_core::router::ModelRouter;
use artemis_core::types::{Message, Role};

fn user_message(content: &str) -> Message {
    Message {
        role: Role::User,
        content: content.to_string(),
        tool_calls: None,
        tool_call_id: None,
        name: None,
    }
}

#[test]
fn test_permissive_fallback_provider_model_format() {
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
}

#[test]
fn test_permissive_fallback_openai_model() {
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
}

#[test]
fn test_permissive_fallback_gemini_model() {
    let router = ModelRouter::new();
    let result = router.resolve("gemini/gemini-2.5-flash", None);
    assert!(
        result.is_ok(),
        "gemini/model format should resolve via permissive fallback"
    );

    let resolved = result.unwrap();
    assert_eq!(resolved.provider, "gemini");
    assert_eq!(resolved.api_protocol, ApiProtocol::GeminiGenerateContent);
}

#[test]
fn test_permissive_fallback_deepseek_model() {
    let router = ModelRouter::new();
    let result = router.resolve("deepseek/deepseek-chat", None);
    assert!(
        result.is_ok(),
        "deepseek/model format should resolve via permissive fallback"
    );

    let resolved = result.unwrap();
    assert_eq!(resolved.provider, "deepseek");
    assert_eq!(resolved.api_protocol, ApiProtocol::OpenAiChat);
}

#[test]
fn test_unknown_model_no_provider_prefix_fails() {
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
}

#[test]
fn test_unknown_provider_prefix_fails() {
    let router = ModelRouter::new();
    let result = router.resolve("nonexistent-provider/some-model", None);
    assert!(result.is_err(), "unknown provider prefix should fail");

    match result.err().unwrap() {
        ArtemisError::ModelNotFound { .. } => {}
        other => panic!("Expected ModelNotFound, got {:?}", other),
    }
}

#[test]
fn test_permissive_resolved_model_fields() {
    let router = ModelRouter::new();
    let resolved = router.resolve_permissive("openai/gpt-4o-mini").unwrap();

    assert_eq!(resolved.provider, "openai");
    assert_eq!(resolved.api_model_id, "gpt-4o-mini");
    assert_eq!(resolved.api_protocol, ApiProtocol::OpenAiChat);
    assert_eq!(resolved.base_url, "https://api.openai.com/v1");
}

#[test]
fn test_permissive_resolved_model_usable_in_chat_request() {
    use artemis_core::provider::ChatRequest;

    let router = ModelRouter::new();
    let resolved = router.resolve("openai/gpt-4o", None).unwrap();

    let request = ChatRequest::new(vec![user_message("Hello")], vec![], resolved.clone());

    assert!(!request.model.is_empty(), "model field should be populated");
    assert_eq!(request.resolved.canonical_id, "gpt-4o");
    assert_eq!(request.resolved.api_protocol, ApiProtocol::OpenAiChat);
}
