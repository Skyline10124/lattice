// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LATTICE LLM vocabulary and functional call facade.

mod client;
mod endpoints;
mod logging;
pub mod provider_projection;
mod providers;
mod router;
pub mod sse;
mod stub;
mod types;

pub use logging::init_logging;
pub use router::{CredentialStatus, ModelRouter, ProviderKind, ResolvedModel};
pub use stub::{LlmError, LlmStream, StubLlm, chat, stream};
pub use types::{
    ChatRequest, ChatResponse, FunctionCall, LlmEvent, Message, ModelTarget, Role, TokenUsage,
    ToolCall, ToolDefinition,
};

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
