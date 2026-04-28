pub mod catalog;
pub mod errors;
pub mod provider;
pub mod providers;
pub mod retry;
pub mod router;
pub mod streaming;
pub mod tokens;
pub mod transport;
pub mod types;

mod mock;

// Re-export key types for convenience
pub use catalog::ResolvedModel;
pub use errors::ArtemisError;
pub use streaming::StreamEvent;
pub use types::{FunctionCall, Message, Role, ToolCall, ToolDefinition};

use router::ModelRouter;

/// Resolve a model name (or alias, e.g. "sonnet") to provider connection details.
/// Credentials are resolved from environment variables.
pub fn resolve(model: &str) -> Result<ResolvedModel, ArtemisError> {
    ModelRouter::new().resolve(model, None)
}

#[cfg(test)]
mod resolve_tests {
    use super::*;

    #[test]
    fn test_resolve_sonnet_alias() {
        let result = resolve("sonnet");
        assert!(result.is_ok());
        let r = result.unwrap();
        assert_eq!(r.canonical_id, "claude-sonnet-4-6");
    }

    #[test]
    fn test_resolve_gpt4o() {
        let result = resolve("gpt-4o");
        assert!(result.is_ok());
        let r = result.unwrap();
        assert_eq!(r.api_protocol, catalog::ApiProtocol::OpenAiChat);
    }

    #[test]
    fn test_resolve_nonexistent_model() {
        let result = resolve("nonexistent-model-xyz-12345");
        assert!(result.is_err());
    }
}
