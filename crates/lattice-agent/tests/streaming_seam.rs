// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

#![cfg(feature = "test-utils")]

use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_llm::testing::ScriptEvent;

#[tokio::test]
async fn fake_stream_preserves_part_order_and_matches_its_live_projection() {
    use lattice_agent::{ModelStepAttempt, ModelStepDriver, ModelStepEvent, ModelStepRequest};
    use lattice_conversation::MessageSemantic;
    use std::sync::Arc;
    let client = FakeStreamingLlmClient::builder()
        .push_events(vec![
            ScriptEvent::Text {
                content: "A".into(),
            },
            ScriptEvent::Reasoning {
                content: "R".into(),
            },
            ScriptEvent::ToolArguments {
                index: 9,
                arguments_delta: "{\"a\":".into(),
            },
            ScriptEvent::Text {
                content: "B".into(),
            },
            ScriptEvent::ToolMetadata {
                index: 2,
                name: "second".into(),
                provider_call_id: None,
            },
            ScriptEvent::ToolArguments {
                index: 2,
                arguments_delta: "{}".into(),
            },
            ScriptEvent::ToolMetadata {
                index: 9,
                name: "first".into(),
                provider_call_id: Some("real-9".into()),
            },
            ScriptEvent::ToolArguments {
                index: 9,
                arguments_delta: "1}".into(),
            },
            ScriptEvent::Done {
                finish_reason: "tool_calls".into(),
                usage: None,
            },
        ])
        .build();
    let mut live = Vec::new();
    let attempt = ModelStepDriver::new(Arc::new(client))
        .run(
            &ModelStepRequest {
                system_prompt: None,
                history: vec![],
                model: "test-model".into(),
                provider: "test".into(),
                tools: vec![],
                cancel: None,
                deadline: None,
                session_id: None,
            },
            Some(&mut |event| match event {
                ModelStepEvent::TokenDelta { content } => live.push(format!("T:{content}")),
                ModelStepEvent::ReasoningDelta { content } => live.push(format!("R:{content}")),
                ModelStepEvent::Opened { .. } => {}
            }),
        )
        .await
        .unwrap();
    let ModelStepAttempt::Complete { message, .. } = attempt else {
        panic!("complete");
    };
    let labels: Vec<_> = message
        .parts
        .iter()
        .map(|part| match &part.semantic {
            MessageSemantic::Text { text } => format!("T:{text}"),
            MessageSemantic::Reasoning { text } => format!("R:{text}"),
            MessageSemantic::ToolCall { call } => format!("C:{}", call.name),
        })
        .collect();
    assert_eq!(labels, ["T:A", "R:R", "C:first", "T:B", "C:second"]);
    assert_eq!(live, ["T:A", "R:R", "T:B"]);
}

fn token(content: &str) -> ScriptEvent {
    ScriptEvent::Text {
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
                ScriptEvent::Failure {
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
                    provider: "test".into(),
                    tools: vec![],
                    cancel: None,
                    deadline: None,
                    session_id: None,
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
