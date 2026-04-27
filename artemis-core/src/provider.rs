#![allow(deprecated)]
use async_trait::async_trait;
use std::collections::HashMap;

use crate::catalog::{ModelCatalogEntry, ResolvedModel};
use crate::errors::ArtemisError;
use crate::router::ModelRouter;
use crate::streaming::{EventStream, TokenUsage};
use crate::types::{Message, ToolCall, ToolDefinition};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur during provider operations.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// A general, non-specific provider error.
    #[error("Provider error: {0}")]
    General(String),

    /// An error returned by the upstream API.
    #[error("API error: {0}")]
    Api(String),

    /// An error during streaming.
    #[error("Stream error: {0}")]
    Stream(String),

    /// The requested provider was not found in the registry.
    #[error("Provider not found: {0}")]
    NotFound(String),
}

// ---------------------------------------------------------------------------
// Request / Response
// ---------------------------------------------------------------------------

/// A request to be sent to an LLM provider.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub model: String,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u32>,
    pub stream: bool,
    pub resolved: ResolvedModel,
}

impl ChatRequest {
    /// Create a new ChatRequest with `model` derived from `resolved.api_model_id`.
    pub fn new(messages: Vec<Message>, tools: Vec<ToolDefinition>, resolved: ResolvedModel) -> Self {
        let model = resolved.api_model_id.clone();
        ChatRequest {
            messages,
            tools,
            model,
            temperature: None,
            max_tokens: None,
            stream: false,
            resolved,
        }
    }
}

/// A response received from an LLM provider.
#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub content: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub usage: Option<TokenUsage>,
    pub finish_reason: String,
    pub model: String,
}

// ---------------------------------------------------------------------------
// Provider trait
// ---------------------------------------------------------------------------

/// Interface that all LLM provider adapters must implement.
///
/// Each concrete provider (OpenAI, Anthropic, etc.) implements this trait
/// and is registered in a [`ModelRegistry`].
#[async_trait]
pub trait Provider: Send + Sync {
    /// Send a chat request and receive a complete (non-streaming) response.
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, ProviderError>;

    /// Send a chat request and receive a streaming response as an [`EventStream`].
    async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> Result<EventStream, ProviderError>;

    /// Human-readable provider name (e.g. `"openai"`, `"anthropic"`).
    fn name(&self) -> &str;

    /// Whether this provider supports streaming responses.
    fn supports_streaming(&self) -> bool;

    /// Whether this provider supports tool / function calling.
    fn supports_tools(&self) -> bool;
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// A single entry in the model registry: catalog entry + provider implementation.
pub struct ModelEntry {
    pub config: ModelCatalogEntry,
    pub provider: Box<dyn Provider>,
}

/// Model-centric registry replacing the old provider-centric `ProviderRegistry`.
///
/// Models are registered by canonical ID. Use [`resolve_and_get`] to resolve
/// a user-facing model name via the router and retrieve the corresponding entry.
///
/// [`resolve_and_get`]: ModelRegistry::resolve_and_get
pub struct ModelRegistry {
    models: HashMap<String, ModelEntry>,
    router: ModelRouter,
}

impl ModelRegistry {
    /// Create a new registry with an empty model map and a fresh [`ModelRouter`].
    pub fn new(router: ModelRouter) -> Self {
        Self {
            models: HashMap::new(),
            router,
        }
    }

    /// Register a model under its canonical ID.
    pub fn register(&mut self, model_id: &str, entry: ModelEntry) {
        self.models.insert(model_id.to_string(), entry);
    }

    /// Look up a registered model by its canonical ID.
    pub fn get(&self, model_id: &str) -> Option<&ModelEntry> {
        self.models.get(model_id)
    }

    /// List all registered model IDs (sorted).
    pub fn list_models(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.models.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// List models that have at least one provider with valid credentials.
    pub fn list_authenticated_models(&self) -> Vec<String> {
        self.router.list_authenticated_models()
    }

    /// Resolve a model name to a [`ModelEntry`] + [`ResolvedModel`] using the router.
    ///
    /// Returns a reference to the registry entry and the resolved model details.
    pub fn resolve_and_get(
        &self,
        model_name: &str,
        provider_override: Option<&str>,
    ) -> Result<(&ModelEntry, ResolvedModel), ArtemisError> {
        let resolved = self
            .router
            .resolve(model_name, provider_override)?;
        let entry = self
            .get(&resolved.canonical_id)
            .ok_or_else(|| ArtemisError::ModelNotFound {
                model: resolved.canonical_id.clone(),
            })?;
        Ok((entry, resolved))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{ApiProtocol, CatalogProviderEntry};
    use crate::types::{Message, Role};
    use std::collections::HashMap;

    /// Minimal mock provider used for registry tests.
    struct MockProvider {
        name: String,
    }

    #[async_trait]
    impl Provider for MockProvider {
        async fn chat(
            &self,
            _request: ChatRequest,
        ) -> Result<ChatResponse, ProviderError> {
            Ok(ChatResponse {
                content: None,
                tool_calls: None,
                usage: None,
                finish_reason: "stop".to_string(),
                model: self.name.clone(),
            })
        }

        async fn chat_stream(
            &self,
            _request: ChatRequest,
        ) -> Result<EventStream, ProviderError> {
            Err(ProviderError::Stream("not implemented in mock".to_string()))
        }

        fn name(&self) -> &str {
            &self.name
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn supports_tools(&self) -> bool {
            true
        }
    }

    fn make_entry(model_id: &str) -> ModelEntry {
        ModelEntry {
            config: ModelCatalogEntry {
                canonical_id: model_id.to_string(),
                display_name: model_id.to_string(),
                description: String::new(),
                context_length: 8192,
                capabilities: vec![],
                providers: vec![CatalogProviderEntry {
                    provider_id: "mock".to_string(),
                    api_model_id: model_id.to_string(),
                    priority: 1,
                    weight: 1,
                    credential_keys: HashMap::new(),
                    base_url: Some("http://localhost".to_string()),
                    api_protocol: ApiProtocol::OpenAiChat,
                    provider_specific: HashMap::new(),
                }],
                aliases: vec![],
            },
            provider: Box::new(MockProvider {
                name: model_id.to_string(),
            }),
        }
    }

    fn make_resolved(model_id: &str) -> ResolvedModel {
        ResolvedModel {
            canonical_id: model_id.to_string(),
            provider: "mock".to_string(),
            api_key: None,
            base_url: "http://localhost".to_string(),
            api_protocol: ApiProtocol::OpenAiChat,
            api_model_id: model_id.to_string(),
            context_length: 8192,
            provider_specific: HashMap::new(),
        }
    }

    #[test]
    fn test_chat_request_new() {
        let messages = vec![Message {
            role: Role::User,
            content: "hello".to_string(),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }];
        let tools = vec![];
        let resolved = make_resolved("test-model");
        let req = ChatRequest::new(messages.clone(), tools.clone(), resolved.clone());
        assert_eq!(req.model, "test-model");
        assert_eq!(req.messages, messages);
        assert_eq!(req.resolved.canonical_id, "test-model");
    }

    #[test]
    fn test_register_and_get() {
        let router = ModelRouter::new();
        let mut registry = ModelRegistry::new(router);
        registry.register("model-1", make_entry("model-1"));

        let retrieved = registry.get("model-1");
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().provider.name(), "model-1");
    }

    #[test]
    fn test_register_multiple_and_list() {
        let router = ModelRouter::new();
        let mut registry = ModelRegistry::new(router);
        registry.register("alpha", make_entry("alpha"));
        registry.register("beta", make_entry("beta"));
        registry.register("gamma", make_entry("gamma"));

        let ids = registry.list_models();
        assert_eq!(ids.len(), 3);
        assert!(ids.contains(&"alpha".to_string()));
        assert!(ids.contains(&"beta".to_string()));
        assert!(ids.contains(&"gamma".to_string()));
    }

    #[test]
    fn test_get_nonexistent_returns_none() {
        let router = ModelRouter::new();
        let registry = ModelRegistry::new(router);
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn test_resolve_and_get_not_registered() {
        let router = ModelRouter::new();
        let registry = ModelRegistry::new(router);
        let result = registry.resolve_and_get("nonexistent-model-xyz", None);
        assert!(result.is_err());
    }
}
