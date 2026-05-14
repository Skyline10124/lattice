use super::*;

#[test]
fn test_parse_raw_sse_single() {
    let input = "event: message\ndata: hello world\n\n";
    let events = parse_raw_sse(input);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event, "message");
    assert_eq!(events[0].data, "hello world");
    assert!(events[0].id.is_none());
}

#[test]
fn test_parse_raw_sse_multi_data() {
    let input = "event: message\ndata: line1\ndata: line2\n\n";
    let events = parse_raw_sse(input);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data, "line1\nline2");
}

#[test]
fn test_parse_raw_sse_multiple_events() {
    let input = "event: first\ndata: 1\n\nevent: second\ndata: 2\n\n";
    let events = parse_raw_sse(input);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event, "first");
    assert_eq!(events[0].data, "1");
    assert_eq!(events[1].event, "second");
    assert_eq!(events[1].data, "2");
}

#[test]
fn test_parse_raw_sse_empty_input() {
    let events = parse_raw_sse("");
    assert!(events.is_empty());
}

#[test]
fn test_parse_raw_sse_no_trailing_newline() {
    let input = "event: test\ndata: hello";
    let events = parse_raw_sse(input);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event, "test");
    assert_eq!(events[0].data, "hello");
}

#[test]
fn test_parse_raw_sse_with_id() {
    let input = "id: 42\nevent: message\ndata: hello\n\n";
    let events = parse_raw_sse(input);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, Some("42".into()));
    assert_eq!(events[0].data, "hello");
}

#[test]
fn test_parse_raw_sse_data_only() {
    let input = "data: hello\n\n";
    let events = parse_raw_sse(input);
    assert_eq!(events.len(), 1);
    assert!(events[0].event.is_empty());
    assert_eq!(events[0].data, "hello");
}

#[test]
fn test_parse_sse_text_openai() {
    let input = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n";
    let mut parser = OpenAiSseParser::new();
    let events = parse_sse_text(input, &mut parser).unwrap();
    assert_eq!(
        events.len(),
        2,
        "[DONE] is a transport signal, no Done event emitted"
    );
    assert!(matches!(events[0], StreamEvent::Token { .. }));
    assert!(matches!(events[1], StreamEvent::Token { .. }));
}

#[test]
fn test_parse_sse_text_anthropic() {
    let input = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"content\":[],\"model\":\"claude-3-5\",\"role\":\"assistant\"}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n\n";
    let mut parser = AnthropicSseParser::new();
    let events = parse_sse_text(input, &mut parser).unwrap();
    assert_eq!(
        events.len(),
        2,
        "message_start is ignored, text delta + Done"
    );
    assert!(matches!(events[0], StreamEvent::Token { .. }));
    assert!(matches!(events[1], StreamEvent::Done { .. }));
}

#[test]
#[ignore = "requires a running SSE HTTP server or mock EventSource"]
fn test_sse_stream_integration() {}

#[test]
fn test_token_usage_roundtrip() {
    let usage = TokenUsage {
        prompt_tokens: 10,
        completion_tokens: 20,
        total_tokens: 30,
    };
    let json = serde_json::to_string(&usage).unwrap();
    let back: TokenUsage = serde_json::from_str(&json).unwrap();
    assert_eq!(usage, back);
}

#[test]
fn test_stream_event_roundtrip() {
    let cases = vec![
        StreamEvent::Token {
            content: "hello".into(),
        },
        StreamEvent::ToolCallStart {
            id: "call_1".into(),
            name: "get_weather".into(),
        },
        StreamEvent::ToolCallDelta {
            id: "call_1".into(),
            arguments_delta: r#"{"loc":"SF"}"#.into(),
        },
        StreamEvent::ToolCallEnd {
            id: "call_1".into(),
        },
        StreamEvent::Done {
            finish_reason: "stop".into(),
            usage: Some(TokenUsage {
                prompt_tokens: 1,
                completion_tokens: 2,
                total_tokens: 3,
            }),
        },
        StreamEvent::Error {
            message: "oops".into(),
        },
    ];

    for event in cases {
        let json = serde_json::to_string(&event).unwrap();
        let back: StreamEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);
    }
}

#[test]
fn test_reasoning_roundtrip() {
    let event = StreamEvent::Reasoning {
        content: "Let me think...".into(),
    };
    let json = serde_json::to_string(&event).unwrap();
    let back: StreamEvent = serde_json::from_str(&json).unwrap();
    assert_eq!(event, back);
}
