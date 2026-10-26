// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LATTICE LLM vocabulary and functional call facade.

mod client;
mod connection;
pub use connection::{ConnectionMaterial, LlmConnection};
mod endpoints;
mod logging;
pub mod protocol;
mod router;
pub mod sse;
mod stub;
mod transport;
mod types;

pub use lattice_conversation::TokenUsage;
pub use logging::init_logging;
pub use router::{
    CredentialStatus, MISSING_OPENAI_COMPATIBLE_MODEL_HINT, ModelRouter, ProviderKind,
    ResolvedModel, credential_env_vars,
};
pub use stub::{LlmError, LlmStream, StubLlm, stream};
pub use transport::{MAX_DIAGNOSTIC_CHARS, bound_diagnostic};
pub use types::{ChatRequest, LlmEvent, ModelTarget, ProviderFailure, ToolDefinition};

// Expose the real parser only for cross-crate streaming boundary tests.
#[cfg(feature = "test-utils")]
pub use protocol::chat_completions::OpenAiSseParser;

#[cfg(test)]
mod test_env {
    use std::sync::{LazyLock, Mutex};

    static ENV_MUTEX: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    pub(crate) fn with_env_vars(vars: &[(&str, Option<&str>)], f: impl FnOnce()) {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|error| error.into_inner());
        let previous: Vec<(&str, Option<String>)> = vars
            .iter()
            .map(|(key, _)| (*key, std::env::var(key).ok()))
            .collect();

        for (key, value) in vars {
            set_env_var(key, *value);
        }

        f();

        for (key, value) in previous {
            set_env_var(key, value.as_deref());
        }
    }

    fn set_env_var(key: &str, value: Option<&str>) {
        // SAFETY: Every environment mutation in this test binary uses ENV_MUTEX,
        // and each mutated key is only observed inside the protected closure.
        unsafe {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}
