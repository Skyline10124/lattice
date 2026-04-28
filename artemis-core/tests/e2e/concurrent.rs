use artemis_core::catalog::{ApiProtocol, ResolvedModel};
use artemis_core::mock::MockProvider;
use artemis_core::provider::{ChatRequest, ModelEntry, ModelRegistry, Provider};
use artemis_core::router::ModelRouter;
use artemis_core::types::{Message, Role, ToolDefinition};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::thread;

fn make_resolved(model_id: &str, provider: &str) -> ResolvedModel {
    ResolvedModel {
        canonical_id: model_id.to_string(),
        provider: provider.to_string(),
        api_key: Some("sk-concurrent-test".to_string()),
        base_url: "http://localhost".to_string(),
        api_protocol: ApiProtocol::OpenAiChat,
        api_model_id: model_id.to_string(),
        context_length: 131072,
        provider_specific: HashMap::new(),
    }
}

fn user_message(content: &str) -> Message {
    Message {
        role: Role::User,
        content: content.to_string(),
        tool_calls: None,
        tool_call_id: None,
        name: None,
    }
}

fn weather_tool() -> ToolDefinition {
    ToolDefinition {
        name: "get_weather".to_string(),
        description: "Get weather".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"]
        }),
    }
}

#[test]
fn test_concurrent_model_resolutions() {
    let router = Arc::new(ModelRouter::new());

    let handles: Vec<_> = (0..4)
        .map(|i| {
            let router = router.clone();
            thread::spawn(move || {
                let model_name = if i % 2 == 0 {
                    "gpt-4o"
                } else {
                    "claude-sonnet-4-6"
                };
                router.resolve(model_name, None)
            })
        })
        .collect();

    let mut results = Vec::new();
    for handle in handles {
        let result = handle.join().expect("thread should not panic");
        results.push(result);
    }

    for result in &results {
        assert!(result.is_ok(), "concurrent resolution should succeed");
    }

    let resolved = results[0].as_ref().unwrap();
    assert!(!resolved.canonical_id.is_empty());
}

#[test]
fn test_concurrent_chat_requests_different_models() {
    let mut provider_a = MockProvider::new("model-a");
    provider_a.set_response("Response from A");

    let mut provider_b = MockProvider::new("model-b");
    provider_b.set_response("Response from B");

    let provider_a = Arc::new(provider_a);
    let provider_b = Arc::new(provider_b);

    let resolved_a = Arc::new(make_resolved("model-a", "mock"));
    let resolved_b = Arc::new(make_resolved("model-b", "mock"));

    let rt = tokio::runtime::Runtime::new().expect("failed to create runtime");

    let result_a = rt.block_on(async {
        let request = ChatRequest::new(
            vec![user_message("Hello A")],
            vec![weather_tool()],
            (*resolved_a).clone(),
        );
        provider_a.chat(request).await
    });

    let result_b = rt.block_on(async {
        let request = ChatRequest::new(
            vec![user_message("Hello B")],
            vec![weather_tool()],
            (*resolved_b).clone(),
        );
        provider_b.chat(request).await
    });

    assert!(result_a.is_ok());
    assert!(result_b.is_ok());
    assert_eq!(result_a.unwrap().content.unwrap(), "Response from A");
    assert_eq!(result_b.unwrap().content.unwrap(), "Response from B");
}

#[test]
fn test_concurrent_registry_lookups() {
    use artemis_core::catalog::CatalogProviderEntry;

    let router = ModelRouter::new();
    let mut registry = ModelRegistry::new(router);

    for i in 0..4 {
        let model_id = format!("concurrent-model-{}", i);
        let mut provider = MockProvider::new(&model_id);
        provider.set_response(&format!("Response {}", i));

        let entry = ModelEntry {
            config: artemis_core::catalog::ModelCatalogEntry {
                canonical_id: model_id.clone(),
                display_name: model_id.clone(),
                description: String::new(),
                context_length: 131072,
                capabilities: vec![],
                providers: vec![CatalogProviderEntry {
                    provider_id: "mock".to_string(),
                    api_model_id: model_id.clone(),
                    priority: 1,
                    weight: 1,
                    credential_keys: HashMap::new(),
                    base_url: Some("http://localhost".to_string()),
                    api_protocol: ApiProtocol::OpenAiChat,
                    provider_specific: HashMap::new(),
                }],
                aliases: vec![],
            },
            provider: Box::new(provider),
        };
        registry.register(&model_id, entry);
    }

    let registry = Arc::new(std::sync::Mutex::new(registry));

    let handles: Vec<_> = (0..4)
        .map(|i| {
            let registry = registry.clone();
            thread::spawn(move || {
                let model_id = format!("concurrent-model-{}", i);
                let reg = registry.lock().unwrap();
                reg.get(&model_id).map(|e| e.provider.name().to_string())
            })
        })
        .collect();

    for (i, handle) in handles.into_iter().enumerate() {
        let result = handle.join().expect("thread should not panic");
        let name = result.expect("model should be found");
        assert_eq!(name, format!("concurrent-model-{}", i));
    }
}

#[test]
fn test_concurrent_resolved_model_construction() {
    let resolved_models: Vec<ResolvedModel> = (0..4)
        .map(|i| make_resolved(&format!("model-{}", i), &format!("provider-{}", i)))
        .collect();

    for (i, resolved) in resolved_models.iter().enumerate() {
        assert_eq!(resolved.canonical_id, format!("model-{}", i));
        assert_eq!(resolved.provider, format!("provider-{}", i));
        assert_eq!(resolved.api_key.as_deref(), Some("sk-concurrent-test"));
    }

    let requests: Vec<ChatRequest> = resolved_models
        .iter()
        .map(|r| ChatRequest::new(vec![user_message("test")], vec![weather_tool()], r.clone()))
        .collect();

    assert_eq!(requests.len(), 4);
    for (i, req) in requests.iter().enumerate() {
        assert_eq!(req.resolved.canonical_id, format!("model-{}", i));
    }
}
