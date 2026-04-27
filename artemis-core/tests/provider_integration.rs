#![allow(deprecated)]
//! Provider adapter integration tests.
//!
//! Verifies the structural properties of all 8 provider adapters:
//! - All providers exist, implement Provider trait, and can be constructed
//! - Each provider reports correct `name()`, `supports_streaming()`, `supports_tools()`
//! - Each provider can build a ChatRequest from a ResolvedModel
//! - Error handling: ProviderError types work correctly
//!
//! No real HTTP calls — these are purely structural/type-level tests.

use artemis_core::catalog::{ApiProtocol, ResolvedModel};
use artemis_core::provider::{ChatRequest, Provider, ProviderError};
use std::collections::HashMap;

// ── Helpers ───────────────────────────────────────────────────────────────

fn make_resolved(provider: &str, model: &str) -> ResolvedModel {
    ResolvedModel {
        canonical_id: model.to_string(),
        provider: provider.to_string(),
        api_key: Some("test-key".to_string()),
        base_url: "http://localhost".to_string(),
        api_protocol: ApiProtocol::OpenAiChat,
        api_model_id: model.to_string(),
        context_length: 131072,
        provider_specific: HashMap::new(),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// All 8 providers exist and implement Provider trait
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_all_eight_providers_exist() {
    // Construct every provider — confirms structs exist and compile-time safety.
    let _openai = artemis_core::providers::openai::OpenAIProvider::new();
    let _anthropic = artemis_core::providers::anthropic::AnthropicProvider::new();
    let _gemini = artemis_core::providers::gemini::GeminiProvider::new();
    let _ollama = artemis_core::providers::ollama::OllamaProvider::new();
    let _groq = artemis_core::providers::groq::GroqProvider::new();
    let _xai = artemis_core::providers::xai::XAIProvider::new();
    let _deepseek = artemis_core::providers::deepseek::DeepSeekProvider::new();
    let _mistral = artemis_core::providers::mistral::MistralProvider::new();
}

// ═══════════════════════════════════════════════════════════════════════════
// Individual provider structural tests
// ═══════════════════════════════════════════════════════════════════════════

mod openai {
    use super::*;
    use artemis_core::providers::openai::OpenAIProvider;

    #[test]
    fn name_streaming_tools() {
        let p = OpenAIProvider::new();
        assert_eq!(p.name(), "openai");
        assert!(p.supports_streaming());
        assert!(p.supports_tools());
    }

    #[test]
    fn can_build_chat_request() {
        let p = OpenAIProvider::new();
        assert_eq!(p.name(), "openai");
        let resolved = make_resolved("openai", "gpt-4o");
        let req = ChatRequest::new(vec![], vec![], resolved);
        assert_eq!(req.model, "gpt-4o");
        assert_eq!(req.resolved.provider, "openai");
    }

    #[test]
    fn default_impl() {
        let p = OpenAIProvider::default();
        assert_eq!(p.name(), "openai");
    }
}

mod anthropic {
    use super::*;
    use artemis_core::providers::anthropic::AnthropicProvider;

    #[test]
    fn name_streaming_tools() {
        let p = AnthropicProvider::new();
        assert_eq!(p.name(), "anthropic");
        assert!(!p.supports_streaming());
        assert!(p.supports_tools());
    }

    #[test]
    fn can_build_chat_request() {
        let p = AnthropicProvider::new();
        assert_eq!(p.name(), "anthropic");
        let resolved = make_resolved("anthropic", "claude-sonnet-4-20250514");
        let req = ChatRequest::new(vec![], vec![], resolved);
        assert_eq!(req.model, "claude-sonnet-4-20250514");
        assert_eq!(req.resolved.provider, "anthropic");
    }

    #[test]
    fn default_impl() {
        let p = AnthropicProvider::default();
        assert_eq!(p.name(), "anthropic");
    }
}

mod gemini {
    use super::*;
    use artemis_core::providers::gemini::GeminiProvider;

    #[test]
    fn name_streaming_tools() {
        let p = GeminiProvider::new();
        assert_eq!(p.name(), "gemini");
        assert!(p.supports_streaming());
        assert!(p.supports_tools());
    }

    #[test]
    fn can_build_chat_request() {
        let p = GeminiProvider::new();
        assert_eq!(p.name(), "gemini");
        let resolved = make_resolved("gemini", "gemini-2.5-flash");
        let req = ChatRequest::new(vec![], vec![], resolved);
        assert_eq!(req.model, "gemini-2.5-flash");
        assert_eq!(req.resolved.provider, "gemini");
    }

    #[test]
    fn default_impl() {
        let p = GeminiProvider::default();
        assert_eq!(p.name(), "gemini");
    }
}

mod ollama {
    use super::*;
    use artemis_core::providers::ollama::OllamaProvider;

    #[test]
    fn name_streaming_tools() {
        let p = OllamaProvider::new();
        assert_eq!(p.name(), "ollama");
        assert!(!p.supports_streaming());
        assert!(p.supports_tools());
    }

    #[test]
    fn can_build_chat_request() {
        let p = OllamaProvider::new();
        assert_eq!(p.name(), "ollama");
        let resolved = make_resolved("ollama", "llama3");
        let req = ChatRequest::new(vec![], vec![], resolved);
        assert_eq!(req.model, "llama3");
        assert_eq!(req.resolved.provider, "ollama");
    }

    #[test]
    fn default_impl() {
        let p = OllamaProvider::default();
        assert_eq!(p.name(), "ollama");
    }
}

mod groq {
    use super::*;
    use artemis_core::providers::groq::GroqProvider;

    #[test]
    fn name_streaming_tools() {
        let p = GroqProvider::new();
        assert_eq!(p.name(), "groq");
        assert!(!p.supports_streaming());
        assert!(p.supports_tools());
    }

    #[test]
    fn can_build_chat_request() {
        let p = GroqProvider::new();
        assert_eq!(p.name(), "groq");
        let resolved = make_resolved("groq", "llama-3.3-70b-versatile");
        let req = ChatRequest::new(vec![], vec![], resolved);
        assert_eq!(req.model, "llama-3.3-70b-versatile");
        assert_eq!(req.resolved.provider, "groq");
    }

    #[test]
    fn default_impl() {
        let p = GroqProvider::default();
        assert_eq!(p.name(), "groq");
    }
}

mod xai {
    use super::*;
    use artemis_core::providers::xai::XAIProvider;

    #[test]
    fn name_streaming_tools() {
        let p = XAIProvider::new();
        assert_eq!(p.name(), "xai");
        assert!(!p.supports_streaming());
        assert!(p.supports_tools());
    }

    #[test]
    fn can_build_chat_request() {
        let p = XAIProvider::new();
        assert_eq!(p.name(), "xai");
        let resolved = make_resolved("xai", "grok-3-beta");
        let req = ChatRequest::new(vec![], vec![], resolved);
        assert_eq!(req.model, "grok-3-beta");
        assert_eq!(req.resolved.provider, "xai");
    }

    #[test]
    fn default_impl() {
        let p = XAIProvider::default();
        assert_eq!(p.name(), "xai");
    }
}

mod deepseek {
    use super::*;
    use artemis_core::providers::deepseek::DeepSeekProvider;

    #[test]
    fn name_streaming_tools() {
        let p = DeepSeekProvider::new();
        assert_eq!(p.name(), "deepseek");
        assert!(p.supports_streaming());
        assert!(p.supports_tools());
    }

    #[test]
    fn can_build_chat_request() {
        let p = DeepSeekProvider::new();
        assert_eq!(p.name(), "deepseek");
        let resolved = make_resolved("deepseek", "deepseek-chat");
        let req = ChatRequest::new(vec![], vec![], resolved);
        assert_eq!(req.model, "deepseek-chat");
        assert_eq!(req.resolved.provider, "deepseek");
    }

    #[test]
    fn default_impl() {
        let p = DeepSeekProvider::default();
        assert_eq!(p.name(), "deepseek");
    }
}

mod mistral {
    use super::*;
    use artemis_core::providers::mistral::MistralProvider;

    #[test]
    fn name_streaming_tools() {
        let p = MistralProvider::new();
        assert_eq!(p.name(), "mistral");
        assert!(p.supports_streaming());
        assert!(p.supports_tools());
    }

    #[test]
    fn can_build_chat_request() {
        let p = MistralProvider::new();
        assert_eq!(p.name(), "mistral");
        let resolved = make_resolved("mistral", "mistral-large-latest");
        let req = ChatRequest::new(vec![], vec![], resolved);
        assert_eq!(req.model, "mistral-large-latest");
        assert_eq!(req.resolved.provider, "mistral");
    }

    #[test]
    fn default_impl() {
        let p = MistralProvider::default();
        assert_eq!(p.name(), "mistral");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Cross-provider consistency
// ═══════════════════════════════════════════════════════════════════════════

/// All 8 providers report unique names.
#[test]
fn test_all_provider_names_are_unique() {
    let providers: Vec<Box<dyn Provider>> = vec![
        Box::new(artemis_core::providers::openai::OpenAIProvider::new()),
        Box::new(artemis_core::providers::anthropic::AnthropicProvider::new()),
        Box::new(artemis_core::providers::gemini::GeminiProvider::new()),
        Box::new(artemis_core::providers::ollama::OllamaProvider::new()),
        Box::new(artemis_core::providers::groq::GroqProvider::new()),
        Box::new(artemis_core::providers::xai::XAIProvider::new()),
        Box::new(artemis_core::providers::deepseek::DeepSeekProvider::new()),
        Box::new(artemis_core::providers::mistral::MistralProvider::new()),
    ];

    let mut names: Vec<&str> = providers.iter().map(|p| p.name()).collect();
    let len_before = names.len();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), len_before, "all provider names must be unique");
}

/// All 8 providers support tools (function calling).
#[test]
fn test_all_providers_support_tools() {
    let providers: Vec<Box<dyn Provider>> = vec![
        Box::new(artemis_core::providers::openai::OpenAIProvider::new()),
        Box::new(artemis_core::providers::anthropic::AnthropicProvider::new()),
        Box::new(artemis_core::providers::gemini::GeminiProvider::new()),
        Box::new(artemis_core::providers::ollama::OllamaProvider::new()),
        Box::new(artemis_core::providers::groq::GroqProvider::new()),
        Box::new(artemis_core::providers::xai::XAIProvider::new()),
        Box::new(artemis_core::providers::deepseek::DeepSeekProvider::new()),
        Box::new(artemis_core::providers::mistral::MistralProvider::new()),
    ];

    for p in &providers {
        assert!(
            p.supports_tools(),
            "provider '{}' should support tools",
            p.name()
        );
    }
}

/// Verify which providers support streaming (non-uniform).
#[test]
fn test_streaming_support_matrix() {
    // Providers that DO support streaming
    assert!(artemis_core::providers::openai::OpenAIProvider::new().supports_streaming());
    assert!(artemis_core::providers::gemini::GeminiProvider::new().supports_streaming());
    assert!(artemis_core::providers::deepseek::DeepSeekProvider::new().supports_streaming());
    assert!(artemis_core::providers::mistral::MistralProvider::new().supports_streaming());

    // Providers that do NOT support streaming
    assert!(!artemis_core::providers::anthropic::AnthropicProvider::new().supports_streaming());
    assert!(!artemis_core::providers::ollama::OllamaProvider::new().supports_streaming());
    assert!(!artemis_core::providers::groq::GroqProvider::new().supports_streaming());
    assert!(!artemis_core::providers::xai::XAIProvider::new().supports_streaming());
}

// ═══════════════════════════════════════════════════════════════════════════
// ChatRequest construction from ResolvedModel
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_chat_request_uses_resolved_api_model_id() {
    let resolved = make_resolved("openai", "gpt-4o");
    let req = ChatRequest::new(vec![], vec![], resolved);
    assert_eq!(req.model, "gpt-4o");
    assert_eq!(req.resolved.api_model_id, "gpt-4o");
    assert_eq!(req.resolved.canonical_id, "gpt-4o");
}

#[test]
fn test_chat_request_defaults_to_no_stream() {
    let resolved = make_resolved("openai", "gpt-4o");
    let req = ChatRequest::new(vec![], vec![], resolved);
    assert!(!req.stream, "stream should default to false");
}

#[test]
fn test_chat_request_stores_resolved_config() {
    let resolved = make_resolved("anthropic", "claude-sonnet-4-20250514");
    let req = ChatRequest::new(vec![], vec![], resolved.clone());
    assert_eq!(req.resolved.provider, resolved.provider);
    assert_eq!(req.resolved.api_key, resolved.api_key);
    assert_eq!(req.resolved.base_url, resolved.base_url);
    assert_eq!(req.resolved.context_length, resolved.context_length);
}

// ═══════════════════════════════════════════════════════════════════════════
// ProviderError type tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_provider_error_general_display() {
    let err = ProviderError::General("something went wrong".to_string());
    let msg = err.to_string();
    assert!(msg.contains("Provider error"), "should contain 'Provider error': {msg}");
    assert!(msg.contains("something went wrong"), "should contain the original message: {msg}");
}

#[test]
fn test_provider_error_api_display() {
    let err = ProviderError::Api("rate limit exceeded".to_string());
    let msg = err.to_string();
    assert!(msg.contains("API error"), "should contain 'API error': {msg}");
    assert!(msg.contains("rate limit exceeded"), "should contain the original message: {msg}");
}

#[test]
fn test_provider_error_stream_display() {
    let err = ProviderError::Stream("connection broken".to_string());
    let msg = err.to_string();
    assert!(msg.contains("Stream error"), "should contain 'Stream error': {msg}");
    assert!(msg.contains("connection broken"), "should contain the original message: {msg}");
}

#[test]
fn test_provider_error_not_found_display() {
    let err = ProviderError::NotFound("unknown-provider".to_string());
    let msg = err.to_string();
    assert!(msg.contains("Provider not found"), "should contain 'Provider not found': {msg}");
    assert!(msg.contains("unknown-provider"), "should contain the provider name: {msg}");
}

#[test]
fn test_provider_error_implements_debug() {
    let err = ProviderError::General("test".to_string());
    let debug = format!("{:?}", err);
    assert!(!debug.is_empty());
}

/// ProviderError should be Send + Sync (required for async runtime).
#[test]
fn test_provider_error_is_send_sync() {
    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}
    assert_send::<ProviderError>();
    assert_sync::<ProviderError>();
}

// ═══════════════════════════════════════════════════════════════════════════
// Provider trait object safety
// ═══════════════════════════════════════════════════════════════════════════

/// Verify that all provider types can be boxed as trait objects.
#[test]
fn test_providers_are_boxable() {
    let _providers: Vec<Box<dyn Provider>> = vec![
        Box::new(artemis_core::providers::openai::OpenAIProvider::new()),
        Box::new(artemis_core::providers::anthropic::AnthropicProvider::new()),
        Box::new(artemis_core::providers::gemini::GeminiProvider::new()),
        Box::new(artemis_core::providers::ollama::OllamaProvider::new()),
        Box::new(artemis_core::providers::groq::GroqProvider::new()),
        Box::new(artemis_core::providers::xai::XAIProvider::new()),
        Box::new(artemis_core::providers::deepseek::DeepSeekProvider::new()),
        Box::new(artemis_core::providers::mistral::MistralProvider::new()),
    ];
}
