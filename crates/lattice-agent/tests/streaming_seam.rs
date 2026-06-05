// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

#![cfg(feature = "test-utils")]

use futures::StreamExt;
use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_agent::LlmClient;
use lattice_llm::{
    ChatRequest, ChatResponse, FunctionCall, LlmEvent, ModelTarget, TokenUsage, ToolCall,
};

fn make_chat_request() -> ChatRequest {
    ChatRequest::new(
        vec![lattice_llm::Message::user("stream please")],
        ModelTarget::new("test-model"),
    )
}

#[derive(Clone)]
struct BufferedClient {
    response: Result<ChatResponse, String>,
}

#[async_trait::async_trait]
impl LlmClient for BufferedClient {
    async fn chat(
        &self,
        _request: ChatRequest,
    ) -> Result<ChatResponse, Box<dyn std::error::Error + Send + Sync>> {
        self.response.clone().map_err(Into::into)
    }
}

fn response_with(
    content: &str,
    reasoning_content: Option<&str>,
    tool_calls: Option<Vec<ToolCall>>,
    finish_reason: &str,
    usage: Option<TokenUsage>,
) -> ChatResponse {
    ChatResponse {
        content: content.to_string(),
        reasoning_content: reasoning_content.map(str::to_string),
        tool_calls,
        finish_reason: finish_reason.to_string(),
        model: "test".to_string(),
        usage,
    }
}

fn done(finish_reason: &str, usage: Option<TokenUsage>) -> LlmEvent {
    LlmEvent::Done {
        finish_reason: finish_reason.to_string(),
        usage,
    }
}

fn token(content: &str) -> LlmEvent {
    LlmEvent::Token {
        content: content.to_string(),
    }
}

#[tokio::test]
async fn default_stream_chat_content_only_emits_token_done() {
    let client = BufferedClient {
        response: Ok(response_with("hi", None, None, "stop", None)),
    };

    let stream = client.stream_chat(make_chat_request()).await.unwrap();
    let events = stream.collect::<Vec<_>>().await;

    assert_eq!(events, vec![token("hi"), done("stop", None)]);
    assert_eq!(events.len(), 2);
}

#[tokio::test]
async fn default_stream_chat_with_reasoning_emits_reasoning_token_done() {
    let client = BufferedClient {
        response: Ok(response_with("hi", Some("think"), None, "stop", None)),
    };

    let stream = client.stream_chat(make_chat_request()).await.unwrap();
    let events = stream.collect::<Vec<_>>().await;

    assert_eq!(
        events,
        vec![
            LlmEvent::Reasoning {
                content: "think".to_string(),
            },
            token("hi"),
            done("stop", None),
        ]
    );
    assert_eq!(events.len(), 3);
}

#[tokio::test]
async fn default_stream_chat_with_tool_call_emits_start_delta_end_done() {
    let tool_call = ToolCall {
        id: "c1".to_string(),
        function: FunctionCall {
            name: "search".to_string(),
            arguments: r#"{"q":"x"}"#.to_string(),
        },
    };
    let client = BufferedClient {
        response: Ok(response_with("", None, Some(vec![tool_call]), "stop", None)),
    };

    let stream = client.stream_chat(make_chat_request()).await.unwrap();
    let events = stream.collect::<Vec<_>>().await;

    assert_eq!(
        events,
        vec![
            LlmEvent::ToolCallStart {
                id: "c1".to_string(),
                name: "search".to_string(),
            },
            LlmEvent::ToolCallDelta {
                id: "c1".to_string(),
                arguments_delta: r#"{"q":"x"}"#.to_string(),
            },
            LlmEvent::ToolCallEnd {
                id: "c1".to_string(),
            },
            done("stop", None),
        ]
    );
    assert_eq!(events.len(), 4);
}

#[tokio::test]
async fn default_stream_chat_skips_tool_call_delta_when_arguments_empty() {
    let tool_call = ToolCall {
        id: "c1".to_string(),
        function: FunctionCall {
            name: "search".to_string(),
            arguments: String::new(),
        },
    };
    let client = BufferedClient {
        response: Ok(response_with("", None, Some(vec![tool_call]), "stop", None)),
    };

    let stream = client.stream_chat(make_chat_request()).await.unwrap();
    let events = stream.collect::<Vec<_>>().await;

    assert_eq!(
        events,
        vec![
            LlmEvent::ToolCallStart {
                id: "c1".to_string(),
                name: "search".to_string(),
            },
            LlmEvent::ToolCallEnd {
                id: "c1".to_string(),
            },
            done("stop", None),
        ]
    );
    assert_eq!(events.len(), 3);
    assert!(!events
        .iter()
        .any(|event| matches!(event, LlmEvent::ToolCallDelta { .. })));
}

#[tokio::test]
async fn default_stream_chat_preserves_finish_reason_and_usage() {
    let usage = TokenUsage {
        prompt_tokens: 3,
        completion_tokens: 5,
        total_tokens: 8,
    };
    let client = BufferedClient {
        response: Ok(response_with(
            "hi",
            None,
            None,
            "tool_calls",
            Some(usage.clone()),
        )),
    };

    let stream = client.stream_chat(make_chat_request()).await.unwrap();
    let events = stream.collect::<Vec<_>>().await;

    assert_eq!(
        events.last(),
        Some(&LlmEvent::Done {
            finish_reason: "tool_calls".to_string(),
            usage: Some(usage),
        })
    );
}

#[tokio::test]
async fn default_stream_chat_returns_err_when_chat_fails() {
    let client = BufferedClient {
        response: Err("boom".to_string()),
    };

    let result = client.stream_chat(make_chat_request()).await;

    match result {
        Ok(_) => panic!("stream_chat unexpectedly returned a stream"),
        Err(error) => assert!(error.to_string().contains("boom")),
    }
}

#[tokio::test]
async fn fake_streaming_yields_successive_sequences_across_calls() {
    let client = FakeStreamingLlmClient::builder()
        .push_events(vec![token("a"), done("stop", None)])
        .push_events(vec![token("b"), done("stop", None)])
        .build();

    let first = client
        .stream_chat(make_chat_request())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    let second = client
        .stream_chat(make_chat_request())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;

    assert_eq!(first, vec![token("a"), done("stop", None)]);
    assert_eq!(second, vec![token("b"), done("stop", None)]);
    assert_eq!(client.call_count(), 2);
}

#[tokio::test]
async fn fake_stream_chat_returns_err_for_queued_startup_error() {
    let client = FakeStreamingLlmClient::builder()
        .push_error("provider unavailable")
        .build();

    let result = client.stream_chat(make_chat_request()).await;

    match result {
        Ok(_) => panic!("stream_chat unexpectedly returned a stream"),
        Err(error) => assert!(error.to_string().contains("provider unavailable")),
    }
    assert_eq!(client.call_count(), 1);
}

#[tokio::test]
async fn fake_stream_chat_yields_llm_error_event_for_mid_stream_error() {
    let client = FakeStreamingLlmClient::builder()
        .push_events(vec![
            token("x"),
            LlmEvent::Error {
                message: "mid".to_string(),
            },
            done("stop", None),
        ])
        .build();

    let stream = client.stream_chat(make_chat_request()).await.unwrap();
    let events = stream.collect::<Vec<_>>().await;

    assert!(events.iter().any(|event| {
        matches!(
            event,
            LlmEvent::Error { message } if message == "mid"
        )
    }));
}

#[tokio::test]
async fn fake_chat_aggregates_queued_events_to_chat_response() {
    let usage = TokenUsage {
        prompt_tokens: 4,
        completion_tokens: 6,
        total_tokens: 10,
    };
    let client = FakeStreamingLlmClient::builder()
        .push_events(vec![
            LlmEvent::Reasoning {
                content: "r".to_string(),
            },
            token("hi"),
            LlmEvent::ToolCallStart {
                id: "c1".to_string(),
                name: "sum".to_string(),
            },
            LlmEvent::ToolCallDelta {
                id: "c1".to_string(),
                arguments_delta: "{}".to_string(),
            },
            LlmEvent::ToolCallEnd {
                id: "c1".to_string(),
            },
            done("stop", Some(usage.clone())),
        ])
        .build();

    let response = client.chat(make_chat_request()).await.unwrap();

    assert_eq!(response.content, "hi");
    assert_eq!(response.reasoning_content, Some("r".to_string()));
    assert_eq!(
        response.tool_calls,
        Some(vec![ToolCall {
            id: "c1".to_string(),
            function: FunctionCall {
                name: "sum".to_string(),
                arguments: "{}".to_string(),
            },
        }])
    );
    assert_eq!(response.finish_reason, "stop");
    assert_eq!(response.usage, Some(usage));
}

#[tokio::test]
async fn fake_chat_ignores_provider_metadata_events() {
    let client = FakeStreamingLlmClient::builder()
        .push_events(vec![
            LlmEvent::Started,
            LlmEvent::Citation {
                index: 0,
                cite_text: "c".to_string(),
            },
            LlmEvent::Signature {
                signature: "s".to_string(),
            },
            LlmEvent::GroundingChunk {
                text: "g".to_string(),
                url: None,
            },
            LlmEvent::SafetyRating {
                category: "harm".to_string(),
                probability: "low".to_string(),
            },
            token("x"),
            done("stop", None),
        ])
        .build();

    let response = client.chat(make_chat_request()).await.unwrap();

    assert_eq!(response.content, "x");
    assert_eq!(response.reasoning_content, None);
    assert_eq!(response.tool_calls, None);
    assert_eq!(response.finish_reason, "stop");
}

#[tokio::test]
async fn default_stream_chat_with_multiple_tool_calls_emits_all_sequentially() {
    let tool_calls = vec![
        ToolCall {
            id: "c1".to_string(),
            function: FunctionCall {
                name: "search".to_string(),
                arguments: r#"{"q":"a"}"#.to_string(),
            },
        },
        ToolCall {
            id: "c2".to_string(),
            function: FunctionCall {
                name: "calc".to_string(),
                arguments: r#"{"expr":"1+1"}"#.to_string(),
            },
        },
    ];
    let client = BufferedClient {
        response: Ok(response_with(
            "",
            None,
            Some(tool_calls),
            "tool_calls",
            None,
        )),
    };

    let stream = client.stream_chat(make_chat_request()).await.unwrap();
    let events = stream.collect::<Vec<_>>().await;

    assert_eq!(
        events,
        vec![
            LlmEvent::ToolCallStart {
                id: "c1".to_string(),
                name: "search".to_string(),
            },
            LlmEvent::ToolCallDelta {
                id: "c1".to_string(),
                arguments_delta: r#"{"q":"a"}"#.to_string(),
            },
            LlmEvent::ToolCallEnd {
                id: "c1".to_string(),
            },
            LlmEvent::ToolCallStart {
                id: "c2".to_string(),
                name: "calc".to_string(),
            },
            LlmEvent::ToolCallDelta {
                id: "c2".to_string(),
                arguments_delta: r#"{"expr":"1+1"}"#.to_string(),
            },
            LlmEvent::ToolCallEnd {
                id: "c2".to_string(),
            },
            done("tool_calls", None),
        ]
    );
}

#[tokio::test]
async fn default_stream_chat_with_content_and_tool_calls_emits_token_before_tools() {
    let tool_call = ToolCall {
        id: "c1".to_string(),
        function: FunctionCall {
            name: "run".to_string(),
            arguments: "{}".to_string(),
        },
    };
    let client = BufferedClient {
        response: Ok(response_with(
            "ok",
            None,
            Some(vec![tool_call]),
            "tool_calls",
            None,
        )),
    };

    let stream = client.stream_chat(make_chat_request()).await.unwrap();
    let events = stream.collect::<Vec<_>>().await;

    assert_eq!(
        events,
        vec![
            token("ok"),
            LlmEvent::ToolCallStart {
                id: "c1".to_string(),
                name: "run".to_string(),
            },
            LlmEvent::ToolCallDelta {
                id: "c1".to_string(),
                arguments_delta: "{}".to_string(),
            },
            LlmEvent::ToolCallEnd {
                id: "c1".to_string(),
            },
            done("tool_calls", None),
        ]
    );
}

#[tokio::test]
async fn default_stream_chat_with_empty_response_emits_only_done() {
    let client = BufferedClient {
        response: Ok(response_with("", None, None, "stop", None)),
    };

    let stream = client.stream_chat(make_chat_request()).await.unwrap();
    let events = stream.collect::<Vec<_>>().await;

    assert_eq!(events, vec![done("stop", None)]);
}

#[tokio::test]
async fn fake_chat_returns_err_when_queue_exhausted() {
    let client = FakeStreamingLlmClient::builder().build();

    let result = client.chat(make_chat_request()).await;

    match result {
        Ok(_) => panic!("chat unexpectedly returned a response"),
        Err(error) => assert!(error.to_string().contains("no more fake responses")),
    }
    assert_eq!(client.call_count(), 1);
}

#[tokio::test]
async fn fake_stream_chat_returns_err_when_queue_exhausted() {
    let client = FakeStreamingLlmClient::builder().build();

    let result = client.stream_chat(make_chat_request()).await;

    match result {
        Ok(_) => panic!("stream_chat unexpectedly returned a stream"),
        Err(error) => assert!(error.to_string().contains("no more fake responses")),
    }
    assert_eq!(client.call_count(), 1);
}
