// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

#![cfg(feature = "test-utils")]

use futures::StreamExt;
use lattice_agent::LlmClient;
use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_conversation::{
    BoundToolCall, MessagePart, MessageRole, MessageSemantic, NativeMessage,
};
use lattice_llm::{ChatRequest, LlmEvent, ModelTarget};

fn request() -> ChatRequest {
    ChatRequest::new(
        vec![lattice_conversation::ConversationItem::Message(
            NativeMessage::<BoundToolCall>::new(
                MessageRole::User,
                None,
                vec![MessagePart::semantic_only(MessageSemantic::Text {
                    text: "stream please".into(),
                })],
            ),
        )],
        ModelTarget::new("test-model"),
    )
}

fn token(content: &str) -> LlmEvent {
    LlmEvent::Token {
        content: content.to_string(),
    }
}

fn done() -> LlmEvent {
    LlmEvent::Done {
        finish_reason: "stop".to_string(),
        usage: None,
    }
}

#[tokio::test]
async fn fake_stream_inserts_one_typed_completion_before_done() {
    let client = FakeStreamingLlmClient::builder()
        .push_events(vec![token("hello"), done()])
        .build();
    let events = client
        .stream_chat(request())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;

    assert!(matches!(&events[0], LlmEvent::Token { content } if content == "hello"));
    assert!(matches!(&events[2], LlmEvent::Done { .. }));
    let LlmEvent::MessageComplete { message } = &events[1] else {
        panic!("missing complete message: {events:?}");
    };
    assert!(matches!(
        message.parts.as_slice(),
        [MessagePart {
            semantic: MessageSemantic::Text { text },
            ..
        }] if text == "hello"
    ));
    assert_eq!(client.call_count(), 1);
}

#[tokio::test]
async fn fake_stream_preserves_successive_fifo_sequences() {
    let client = FakeStreamingLlmClient::builder()
        .push_events(vec![token("a"), done()])
        .push_events(vec![token("b"), done()])
        .build();

    for expected in ["a", "b"] {
        let events = client
            .stream_chat(request())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        assert!(matches!(&events[0], LlmEvent::Token { content } if content == expected));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, LlmEvent::MessageComplete { .. }))
                .count(),
            1
        );
    }
    assert_eq!(client.call_count(), 2);
}

#[tokio::test]
async fn fake_stream_distinguishes_startup_and_in_band_errors() {
    let startup = FakeStreamingLlmClient::builder()
        .push_error("provider unavailable")
        .build();
    let error = match startup.stream_chat(request()).await {
        Ok(_) => panic!("startup error unexpectedly opened a stream"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("provider unavailable"));

    let in_band = FakeStreamingLlmClient::builder()
        .push_events(vec![
            token("partial"),
            LlmEvent::Error {
                failure: lattice_llm::ProviderFailure {
                    message: "mid-stream".into(),
                    retryable: false,
                },
            },
            done(),
        ])
        .build();
    let events = in_band
        .stream_chat(request())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, LlmEvent::Error { failure } if failure.message == "mid-stream" && !failure.retryable))
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, LlmEvent::MessageComplete { .. }))
    );
}

#[tokio::test]
async fn fake_stream_rejects_invalid_tool_batches_whole() {
    let client = FakeStreamingLlmClient::builder()
        .push_events(vec![
            LlmEvent::ToolCallStart {
                index: 0,
                provider_call_id: None,
                name: "observe.workspace".to_string(),
            },
            LlmEvent::ToolCallDelta {
                index: 0,
                arguments_delta: "not json".to_string(),
            },
            LlmEvent::ToolCallEnd { index: 0 },
            done(),
        ])
        .build();
    let events = client
        .stream_chat(request())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, LlmEvent::MessageRejected { .. }))
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, LlmEvent::MessageComplete { .. }))
    );
}

#[tokio::test]
async fn exhausted_fake_stream_fails_before_opening() {
    let client = FakeStreamingLlmClient::builder().build();
    let error = match client.stream_chat(request()).await {
        Ok(_) => panic!("empty queue unexpectedly opened a stream"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("no more fake responses"));
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
