// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

#![cfg(feature = "test-utils")]

use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_llm::LlmEvent;

fn token(content: &str) -> LlmEvent {
    LlmEvent::Token {
        content: content.to_string(),
    }
}

#[tokio::test]
async fn typed_stream_failure_after_delta_preserves_retryability() {
    use lattice_agent::{ModelStepAttempt, ModelStepDriver, ModelStepRequest, ModelStopCause};
    for retryable in [false, true] {
        let client = FakeStreamingLlmClient::builder()
            .push_events(vec![
                token("partial"),
                LlmEvent::Error {
                    failure: lattice_llm::ProviderFailure {
                        message: "already redacted".into(),
                        retryable,
                    },
                },
            ])
            .build();
        let result = ModelStepDriver::new(std::sync::Arc::new(client))
            .run(
                &ModelStepRequest {
                    system_prompt: None,
                    history: vec![],
                    model: "test-model".into(),
                    provider: None,
                    tools: vec![],
                    temperature: None,
                    max_tokens: None,
                    cancel: None,
                    deadline: None,
                },
                None,
            )
            .await
            .unwrap();
        assert!(
            matches!(result, ModelStepAttempt::Stopped { cause: ModelStopCause::ProviderError { retryable: actual }, diagnostic, .. } if actual == retryable && diagnostic.provider_error.as_deref() == Some("already redacted"))
        );
    }
}
