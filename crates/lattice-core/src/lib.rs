pub mod behavior;
pub mod catalog;
pub mod errors;
pub mod handoff;
mod invocation;
pub mod logging;
pub mod memory;
pub mod provider;
pub mod retry;
pub mod router;
pub mod security;
pub mod streaming;
pub mod tokens;
pub mod transport;
pub mod types;

// Re-export key types for convenience
pub use catalog::{CredentialStatus, ResolvedModel};
pub use errors::LatticeError;
pub use handoff::{eval_rules, HandoffCondition, HandoffRule, HandoffTarget};
pub use invocation::{chat, chat_complete, chat_with_effort};
pub use logging::{init_debug_logging, init_logging};
pub use streaming::StreamEvent;
pub use types::{
    BehaviorMode, FunctionCall, Message, Role, ToolCall, ToolDefinition, YoloSandboxPolicy,
};

use router::ModelRouter;

/// Resolve a model name (or alias, e.g. "sonnet") to provider connection details.
///
/// This is a stateless convenience — each call creates a fresh router.
/// For custom model registrations, use [`ModelRouter`] directly.
///
/// Credentials are resolved from environment variables.
pub fn resolve(model: &str) -> Result<ResolvedModel, LatticeError> {
    ModelRouter::new().resolve(model, None)
}

/// Inspect a model without requiring it to be callable.
///
/// Unlike `resolve()`, this allows returning models with Missing credentials
/// and empty base_url. Useful for diagnostic commands (`doctor`, `debug`).
pub fn inspect_model(model: &str) -> Result<ResolvedModel, LatticeError> {
    ModelRouter::new().inspect_model(model)
}

#[cfg(test)]
mod resolve_tests {
    use super::*;
    use crate::router::test_support::{restore_all, save_and_clear_all, ENV_MUTEX};

    #[test]
    fn test_resolve_sonnet_alias_missing_credential() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = save_and_clear_all();
        let result = resolve("sonnet");
        match result {
            Ok(r) => panic!("unexpected Ok: provider={}, api_key redacted", r.provider,),
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("API_KEY") || msg.contains("requires"),
                    "error should mention missing credential, got: {}",
                    msg
                );
            }
        }
        restore_all(&saved);
    }

    #[test]
    fn test_resolve_sonnet_alias_with_credential() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = save_and_clear_all();
        std::env::set_var("ANTHROPIC_API_KEY", "sk-test");
        let result = resolve("sonnet");
        assert!(result.is_ok());
        if let Ok(r) = result {
            assert_eq!(r.canonical_id, "claude-sonnet-4-6");
        }
        restore_all(&saved);
    }

    #[test]
    fn resolve_gpt4o_missing_credential_errors() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = save_and_clear_all();
        let result = resolve("gpt-4o");
        match result {
            Ok(r) => panic!("unexpected Ok: provider={}, api_key redacted", r.provider,),
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("API_KEY") || msg.contains("requires"),
                    "error should mention missing credential, got: {}",
                    msg
                );
            }
        }
        restore_all(&saved);
    }

    #[test]
    fn test_resolve_gpt4o_with_key_ok() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = save_and_clear_all();
        std::env::set_var("OPENAI_API_KEY", "sk-test");
        let result = resolve("gpt-4o");
        assert!(result.is_ok());
        if let Ok(r) = result {
            assert_eq!(r.api_protocol, catalog::ApiProtocol::OpenAiChat);
        }
        restore_all(&saved);
    }

    #[test]
    fn test_resolve_nonexistent_model() {
        let result = resolve("nonexistent-model-xyz-12345");
        assert!(result.is_err());
    }
}
