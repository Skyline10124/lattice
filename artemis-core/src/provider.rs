#![allow(deprecated)]
use async_trait::async_trait;
use std::collections::HashMap;

use crate::streaming::{EventStream, TokenUsage};
use crate::types::{Message, ProviderConfig, ToolCall, ToolDefinition};

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
    pub provider_config: ProviderConfig,
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
/// and is registered in a [`ProviderRegistry`].
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

/// A registry of named [`Provider`] implementations.
///
/// Providers are registered by name and can be looked up at runtime.
pub struct ProviderRegistry {
    providers: HashMap<String, Box<dyn Provider>>,
}

impl ProviderRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
        }
    }

    /// Register a provider under the given name.
    pub fn register(&mut self, name: &str, provider: Box<dyn Provider>) {
        self.providers.insert(name.to_string(), provider);
    }

    /// Look up a registered provider by name.
    pub fn get(&self, name: &str) -> Option<&dyn Provider> {
        self.providers.get(name).map(|p| p.as_ref())
    }

    /// List all registered provider names (sorted).
    pub fn list(&self) -> Vec<String> {
        let mut names: Vec<String> = self.providers.keys().cloned().collect();
        names.sort();
        names
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn test_register_and_get() {
        let mut registry = ProviderRegistry::new();
        registry.register(
            "mock1",
            Box::new(MockProvider {
                name: "mock1".to_string(),
            }),
        );

        let retrieved = registry.get("mock1");
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().name(), "mock1");
    }

    #[test]
    fn test_register_multiple_and_list() {
        let mut registry = ProviderRegistry::new();
        registry.register(
            "alpha",
            Box::new(MockProvider {
                name: "alpha".to_string(),
            }),
        );
        registry.register(
            "beta",
            Box::new(MockProvider {
                name: "beta".to_string(),
            }),
        );
        registry.register(
            "gamma",
            Box::new(MockProvider {
                name: "gamma".to_string(),
            }),
        );

        let names = registry.list();
        assert_eq!(names.len(), 3);
        assert!(names.contains(&"alpha".to_string()));
        assert!(names.contains(&"beta".to_string()));
        assert!(names.contains(&"gamma".to_string()));
    }

    #[test]
    fn test_get_nonexistent_returns_none() {
        let registry = ProviderRegistry::new();
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn test_default_is_empty() {
        let registry = ProviderRegistry::default();
        assert!(registry.list().is_empty());
    }
}
