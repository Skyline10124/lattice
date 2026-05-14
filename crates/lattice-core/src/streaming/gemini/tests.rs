use super::*;

#[test]
fn test_done_sentinel() {
    let mut parser = GeminiSseParser::new();
    let events = parser.parse_chunk("", "[DONE]").unwrap();
    assert!(events.is_empty());
}

#[test]
fn test_empty_data() {
    let mut parser = GeminiSseParser::new();
    let events = parser.parse_chunk("", "").unwrap();
    assert!(events.is_empty());
}

#[test]
fn test_error_chunk() {
    let mut parser = GeminiSseParser::new();
    let chunk = r#"{"error":{"message":"API key invalid","code":401}}"#;
    let events = parser.parse_chunk("", chunk).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0],
        StreamEvent::Error {
            message: "API key invalid".into()
        }
    );
}

#[test]
fn test_text_chunk() {
    let mut parser = GeminiSseParser::new();
    let chunk = r#"{"candidates":[{"content":{"parts":[{"text":"Hello world"}],"role":"model"}}]}"#;
    let events = parser.parse_chunk("", chunk).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0],
        StreamEvent::Token {
            content: "Hello world".into()
        }
    );
}

#[test]
fn test_thinking_chunk() {
    let mut parser = GeminiSseParser::new();
    let chunk = r#"{"candidates":[{"content":{"parts":[{"text":"I should check...","thought":true}],"role":"model"}}]}"#;
    let events = parser.parse_chunk("", chunk).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0],
        StreamEvent::Reasoning {
            content: "I should check...".into()
        }
    );
}

#[test]
fn test_function_call_chunk() {
    let mut parser = GeminiSseParser::new();
    let chunk = r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"get_weather","args":{"city":"Tokyo"}}}],"role":"model"}}]}"#;
    let events = parser.parse_chunk("", chunk).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[0],
        StreamEvent::ToolCallStart {
            id: "tc_get_weather_0".into(),
            name: "get_weather".into()
        }
    );
    assert_eq!(
        events[1],
        StreamEvent::ToolCallDelta {
            id: "tc_get_weather_0".into(),
            arguments_delta: r#"{"city":"Tokyo"}"#.into()
        }
    );
}

#[test]
fn test_finish_with_tool_calls() {
    let mut parser = GeminiSseParser::new();
    let chunk1 = r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"search","args":{"q":"rust"}}}],"role":"model"}}]}"#;
    parser.parse_chunk("", chunk1).unwrap();

    let chunk2 = r#"{"candidates":[{"content":{"role":"model"},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":20,"totalTokenCount":30}}"#;
    let events = parser.parse_chunk("", chunk2).unwrap();

    assert!(events.len() >= 2);
    assert_eq!(
        events[0],
        StreamEvent::ToolCallEnd {
            id: "tc_search_0".into()
        }
    );
    let done = events
        .iter()
        .find(|e| matches!(e, StreamEvent::Done { .. }));
    assert!(done.is_some());
    if let StreamEvent::Done {
        finish_reason,
        usage,
    } = done.unwrap()
    {
        assert_eq!(finish_reason, "tool_calls");
        let u = usage.as_ref().unwrap();
        assert_eq!(u.prompt_tokens, 10);
        assert_eq!(u.completion_tokens, 20);
        assert_eq!(u.total_tokens, 30);
    }
}

#[test]
fn test_finish_reason_mapping() {
    let mut parser = GeminiSseParser::new();
    let chunk = r#"{"candidates":[{"content":{"parts":[{"text":"Done"}],"role":"model"},"finishReason":"MAX_TOKENS"}]}"#;
    let events = parser.parse_chunk("", chunk).unwrap();
    let done = events
        .iter()
        .find(|e| matches!(e, StreamEvent::Done { .. }));
    assert!(done.is_some());
    if let StreamEvent::Done { finish_reason, .. } = done.unwrap() {
        assert_eq!(finish_reason, "length");
    }
}

#[test]
fn test_usage_only_chunk() {
    let mut parser = GeminiSseParser::new();
    let chunk =
        r#"{"usageMetadata":{"promptTokenCount":5,"candidatesTokenCount":3,"totalTokenCount":8}}"#;
    let events = parser.parse_chunk("", chunk).unwrap();
    let done = events
        .iter()
        .find(|e| matches!(e, StreamEvent::Done { .. }));
    assert!(done.is_some());
    if let StreamEvent::Done {
        finish_reason,
        usage,
    } = done.unwrap()
    {
        assert_eq!(finish_reason, "stop");
        let u = usage.as_ref().unwrap();
        assert_eq!(u.total_tokens, 8);
    }
}
