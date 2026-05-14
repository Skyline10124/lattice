use super::*;

#[test]
fn test_text_streaming() {
    let mut parser = AnthropicSseParser::new();

    let events = parser
        .parse_chunk(
            "message_start",
            r#"{"type":"message_start","message":{"id":"msg_1","content":[],"model":"claude-3-5-sonnet","role":"assistant","stop_reason":null,"usage":{"input_tokens":10,"output_tokens":1}}}"#,
        )
        .unwrap();
    assert!(events.is_empty());

    let events = parser
        .parse_chunk(
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        )
        .unwrap();
    assert!(events.is_empty());

    let events = parser
        .parse_chunk(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
        )
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0],
        StreamEvent::Token {
            content: "Hello".into()
        }
    );

    let events = parser
        .parse_chunk(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" world"}}"#,
        )
        .unwrap();
    assert_eq!(
        events[0],
        StreamEvent::Token {
            content: " world".into()
        }
    );

    let events = parser
        .parse_chunk(
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
        )
        .unwrap();
    assert!(events.is_empty());

    let events = parser
        .parse_chunk(
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":50}}"#,
        )
        .unwrap();
    assert_eq!(events.len(), 1);
    match &events[0] {
        StreamEvent::Done {
            finish_reason,
            usage,
        } => {
            assert_eq!(finish_reason, "stop", "end_turn maps to stop");
            let usage = usage.as_ref().expect("expected usage");
            assert_eq!(usage.prompt_tokens, 10, "input_tokens from message_start");
            assert_eq!(usage.completion_tokens, 50);
            assert_eq!(usage.total_tokens, 60, "total = input + output");
        }
        other => panic!("expected Done, got {other:?}"),
    }
}

#[test]
fn test_tool_call_streaming() {
    let mut parser = AnthropicSseParser::new();

    let events = parser
        .parse_chunk(
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"get_weather","input":{}}}"#,
        )
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0],
        StreamEvent::ToolCallStart {
            id: "toolu_1".into(),
            name: "get_weather".into(),
        }
    );

    let events = parser
        .parse_chunk(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"location\":\"San"}}"#,
        )
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0],
        StreamEvent::ToolCallDelta {
            id: "toolu_1".into(),
            arguments_delta: r#"{"location":"San"#.into(),
        }
    );

    let events = parser
        .parse_chunk(
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
        )
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0],
        StreamEvent::ToolCallEnd {
            id: "toolu_1".into()
        }
    );
}

#[test]
fn test_ping_ignored() {
    let mut parser = AnthropicSseParser::new();
    let events = parser.parse_chunk("ping", "{}").unwrap();
    assert!(events.is_empty());
}

#[test]
fn test_error_event() {
    let mut parser = AnthropicSseParser::new();
    let events = parser
        .parse_chunk(
            "error",
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        )
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0],
        StreamEvent::Error {
            message: "Overloaded".into()
        }
    );
}

#[test]
fn test_usage_tracks_input_tokens() {
    let mut parser = AnthropicSseParser::new();

    parser
        .parse_chunk(
            "message_start",
            r#"{"type":"message_start","message":{"id":"msg_1","content":[],"model":"claude-3-5-sonnet","role":"assistant","usage":{"input_tokens":42,"output_tokens":1}}}"#,
        )
        .unwrap();

    let events = parser
        .parse_chunk(
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":50}}"#,
        )
        .unwrap();
    assert_eq!(events.len(), 1);
    match &events[0] {
        StreamEvent::Done { usage, .. } => {
            let u = usage.as_ref().expect("expected usage");
            assert_eq!(
                u.prompt_tokens, 42,
                "input_tokens should come from message_start"
            );
            assert_eq!(u.completion_tokens, 50);
            assert_eq!(u.total_tokens, 92, "total = input + output");
        }
        other => panic!("expected Done, got {other:?}"),
    }
}

#[test]
fn test_message_stop_ignored() {
    let mut parser = AnthropicSseParser::new();
    let events = parser
        .parse_chunk("message_stop", r#"{"type":"message_stop"}"#)
        .unwrap();
    assert!(events.is_empty());
}

#[test]
fn test_empty_data_is_noop() {
    let mut parser = AnthropicSseParser::new();
    let events = parser.parse_chunk("ping", "").unwrap();
    assert!(events.is_empty());
}
