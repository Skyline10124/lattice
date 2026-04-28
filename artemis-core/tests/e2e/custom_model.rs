use artemis_core::catalog::{ApiProtocol, CatalogProviderEntry, ModelCatalogEntry};
use artemis_core::mock::MockProvider;
use artemis_core::provider::{ModelEntry, ModelRegistry};
use artemis_core::router::ModelRouter;
use std::collections::HashMap;

#[test]
fn test_register_custom_model_resolve_and_call() {
    let mut router = ModelRouter::new();
    let custom_entry = ModelCatalogEntry {
        canonical_id: "my-custom-llm".to_string(),
        display_name: "My Custom LLM".to_string(),
        description: "Custom model for internal use".to_string(),
        context_length: 4096,
        capabilities: vec!["chat".to_string()],
        providers: vec![CatalogProviderEntry {
            provider_id: "custom".to_string(),
            api_model_id: "my-llm".to_string(),
            priority: 1,
            weight: 1,
            credential_keys: HashMap::from([("api_key".to_string(), "MY_LLM_API_KEY".to_string())]),
            base_url: Some("http://my-llm.internal:8080/v1".to_string()),
            api_protocol: ApiProtocol::OpenAiChat,
            provider_specific: HashMap::new(),
        }],
        aliases: vec!["myllm".to_string()],
    };
    router.register_model(custom_entry);

    let models = router.list_models();
    assert!(
        models.contains(&"my-custom-llm".to_string()),
        "custom model should appear in list"
    );

    let resolved = router
        .resolve("my-custom-llm", None)
        .expect("should resolve custom model");
    assert_eq!(resolved.canonical_id, "my-custom-llm");
    assert_eq!(resolved.provider, "custom");
    assert_eq!(resolved.api_model_id, "my-llm");
    assert_eq!(resolved.api_protocol, ApiProtocol::OpenAiChat);
    assert_eq!(resolved.base_url, "http://my-llm.internal:8080/v1");
}

#[test]
fn test_register_custom_model_alias_resolution() {
    let mut router = ModelRouter::new();
    let custom_entry = ModelCatalogEntry {
        canonical_id: "my-custom-llm-2".to_string(),
        display_name: "My Custom LLM 2".to_string(),
        description: String::new(),
        context_length: 8192,
        capabilities: vec![],
        providers: vec![CatalogProviderEntry {
            provider_id: "custom".to_string(),
            api_model_id: "my-llm".to_string(),
            priority: 1,
            weight: 1,
            credential_keys: HashMap::new(),
            base_url: Some("http://localhost:11434/v1".to_string()),
            api_protocol: ApiProtocol::OpenAiChat,
            provider_specific: HashMap::new(),
        }],
        aliases: vec!["myllm2".to_string(), "myllm2alt".to_string()],
    };
    router.register_model(custom_entry);

    let resolved = router
        .resolve("myllm2", None)
        .expect("alias should resolve");
    assert_eq!(resolved.canonical_id, "my-custom-llm-2");

    let resolved2 = router
        .resolve("myllm2alt", None)
        .expect("second alias should resolve");
    assert_eq!(resolved2.canonical_id, "my-custom-llm-2");
}

#[test]
fn test_custom_model_in_registry_resolve_and_get() {
    let mut router = ModelRouter::new();
    let custom_entry = ModelCatalogEntry {
        canonical_id: "test-custom-reg".to_string(),
        display_name: "Test Custom".to_string(),
        description: String::new(),
        context_length: 8192,
        capabilities: vec![],
        providers: vec![CatalogProviderEntry {
            provider_id: "custom".to_string(),
            api_model_id: "test-custom-reg".to_string(),
            priority: 1,
            weight: 1,
            credential_keys: HashMap::new(),
            base_url: Some("http://localhost:1234/v1".to_string()),
            api_protocol: ApiProtocol::OpenAiChat,
            provider_specific: HashMap::new(),
        }],
        aliases: vec![],
    };

    let mut provider = MockProvider::new("test-custom-reg");
    provider.set_response("Custom model response!");

    router.register_model(custom_entry.clone());

    let mut registry = ModelRegistry::new(router);
    let model_entry = ModelEntry {
        config: custom_entry,
        provider: Box::new(provider),
    };
    registry.register("test-custom-reg", model_entry);

    let (entry, resolved) = registry
        .resolve_and_get("test-custom-reg", None)
        .expect("should resolve and get custom model");
    assert_eq!(entry.provider.name(), "test-custom-reg");
    assert_eq!(resolved.canonical_id, "test-custom-reg");
}

#[test]
fn test_custom_model_anthropic_protocol() {
    let mut router = ModelRouter::new();
    let custom_entry = ModelCatalogEntry {
        canonical_id: "claude-internal".to_string(),
        display_name: "Internal Claude".to_string(),
        description: String::new(),
        context_length: 200000,
        capabilities: vec![],
        providers: vec![CatalogProviderEntry {
            provider_id: "anthropic".to_string(),
            api_model_id: "claude-sonnet-4-6".to_string(),
            priority: 1,
            weight: 1,
            credential_keys: HashMap::new(),
            base_url: Some("https://internal-anthropic.corp".to_string()),
            api_protocol: ApiProtocol::AnthropicMessages,
            provider_specific: HashMap::new(),
        }],
        aliases: vec!["internal-claude".to_string()],
    };
    router.register_model(custom_entry);

    let resolved = router
        .resolve("claude-internal", None)
        .expect("should resolve");
    assert_eq!(resolved.api_protocol, ApiProtocol::AnthropicMessages);
    assert_eq!(resolved.base_url, "https://internal-anthropic.corp");
}
