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

pub use lattice_conversation::TokenUsage;
pub use logging::init_logging;
pub use router::{CredentialStatus, ModelRouter, ProviderKind, ResolvedModel};
pub use stub::{LlmError, LlmStream, StubLlm, stream};
pub use types::{ChatRequest, LlmEvent, ModelTarget, ToolDefinition};

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

#[cfg(test)]
mod production_codec_boundary {
    #[test]
    fn every_provider_delegates_request_and_response_projection() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/providers");
        for (file, response_marker) in [
            ("openai.rs", "wire_to_native"),
            ("anthropic.rs", "wire_to_native"),
            ("gemini.rs", "complete_message_event"),
        ] {
            let source = std::fs::read_to_string(root.join(file)).expect("provider source");
            let production = source
                .split("#[cfg(test)]")
                .next()
                .expect("production prefix");
            assert!(
                production.contains("request_messages_to_wire"),
                "{file} bypasses the sole request projector"
            );
            assert!(
                production.contains(response_marker),
                "{file} bypasses the sole completed-response projector"
            );
            assert!(
                production.contains("wire_usage"),
                "{file} bypasses the sole usage projector"
            );
            assert!(!production.contains("assemble_request_messages"));
            assert!(!production.contains("tc_"));
        }
    }
}
