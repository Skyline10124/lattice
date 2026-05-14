use super::*;

#[test]
fn test_done_sentinel() {
    let mut parser = OpenAiSseParser::new();
    let events = parser.parse_chunk("message", "[DONE]").unwrap();
    assert!(
        events.is_empty(),
        "[DONE] is a transport signal only, not a semantic event"
    );
}

#[test]
fn test_content_chunk() {
    let mut parser = OpenAiSseParser::new();
    let chunk = r#"{"id":"chatcmpl-9a1","object":"chat.completion.chunk","created":1700000000,"model":"gpt-4o","choices":[{"index":0,"delta":{"role":"assistant","content":"Hello"},"finish_reason":null}]}"#;
    let events = parser.parse_chunk("message", chunk).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0],
        StreamEvent::Token {
            content: "Hello".into()
        }
    );
}

#[test]
fn test_multiple_content_chunks() {
    let mut parser = OpenAiSseParser::new();
    let chunks = vec![
        r#"{"choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"content":" world"},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
    ];

    let mut all_events = Vec::new();
    for chunk in &chunks {
        let events = parser.parse_chunk("message", chunk).unwrap();
        all_events.extend(events);
    }

    assert_eq!(all_events.len(), 3);
    assert_eq!(
        all_events[0],
        StreamEvent::Token {
            content: "Hello".into()
        }
    );
    assert_eq!(
        all_events[1],
        StreamEvent::Token {
            content: " world".into()
        }
    );
    assert!(matches!(&all_events[2], StreamEvent::Done { .. }));
}

#[test]
fn test_empty_delta_skipped() {
    let mut parser = OpenAiSseParser::new();
    let chunk = r#"{"choices":[{"index":0,"delta":{},"finish_reason":null}]}"#;
    let events = parser.parse_chunk("message", chunk).unwrap();
    assert!(events.is_empty());
}

#[test]
fn test_tool_call_streaming() {
    let mut parser = OpenAiSseParser::new();
    let chunks = vec![
        r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":null,"tool_calls":[{"index":0,"id":"call_abc123","type":"function","function":{"name":"get_weather","arguments":""}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"location\":\"San"}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":" Francisco\"}"}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
    ];

    let mut all_events = Vec::new();
    for chunk in &chunks {
        let events = parser.parse_chunk("message", chunk).unwrap();
        all_events.extend(events);
    }

    assert_eq!(all_events.len(), 5);
    assert_eq!(
        all_events[0],
        StreamEvent::ToolCallStart {
            id: "call_abc123".into(),
            name: "get_weather".into(),
        }
    );
    assert_eq!(
        all_events[1],
        StreamEvent::ToolCallDelta {
            id: "call_abc123".into(),
            arguments_delta: r#"{"location":"San"#.into(),
        }
    );
    assert_eq!(
        all_events[2],
        StreamEvent::ToolCallDelta {
            id: "call_abc123".into(),
            arguments_delta: " Francisco\"}".into(),
        }
    );
    assert_eq!(
        all_events[3],
        StreamEvent::ToolCallEnd {
            id: "call_abc123".into()
        }
    );
    assert_eq!(
        all_events[4],
        StreamEvent::Done {
            finish_reason: "tool_calls".into(),
            usage: None,
        }
    );
}

#[test]
fn test_api_error() {
    let mut parser = OpenAiSseParser::new();
    let chunk = r#"{"error":{"message":"Insufficient quota","type":"insufficient_quota","code":"insufficient_quota"}}"#;
    let events = parser.parse_chunk("message", chunk).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0],
        StreamEvent::Error {
            message: "Insufficient quota".into()
        }
    );
}

#[test]
fn test_done_with_usage() {
    let mut parser = OpenAiSseParser::new();
    let chunk = r#"{"id":"chatcmpl-9a1","object":"chat.completion.chunk","created":1700000000,"model":"gpt-4o","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":20,"total_tokens":30}}"#;
    let events = parser.parse_chunk("message", chunk).unwrap();
    assert_eq!(events.len(), 1);
    match &events[0] {
        StreamEvent::Done {
            finish_reason,
            usage,
        } => {
            assert_eq!(finish_reason, "stop");
            let usage = usage.as_ref().expect("expected usage");
            assert_eq!(usage.prompt_tokens, 10);
            assert_eq!(usage.completion_tokens, 20);
            assert_eq!(usage.total_tokens, 30);
        }
        other => panic!("expected Done, got {other:?}"),
    }
}

#[test]
fn test_whitespace_done() {
    let mut parser = OpenAiSseParser::new();
    let events = parser.parse_chunk("message", "  [DONE]  ").unwrap();
    assert!(
        events.is_empty(),
        "[DONE] with whitespace is still a transport signal"
    );
}

#[test]
fn test_very_large_content_chunk() {
    let mut parser = OpenAiSseParser::new();
    let content = "A".repeat(10_000);
    let chunk = format!(
        r#"{{"choices":[{{"index":0,"delta":{{"content":"{content}"}},"finish_reason":null}}]}}"#
    );
    let events = parser.parse_chunk("message", &chunk).unwrap();
    assert_eq!(events.len(), 1);
    match &events[0] {
        StreamEvent::Token { content: c } => assert_eq!(c.len(), 10_000),
        other => panic!("expected Token, got {other:?}"),
    }
}

#[test]
fn test_multiple_tool_calls() {
    let mut parser = OpenAiSseParser::new();
    let chunks = vec![
        r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":null,"tool_calls":[
            {"index":0,"id":"call_a","type":"function","function":{"name":"fn_a","arguments":""}},
            {"index":1,"id":"call_b","type":"function","function":{"name":"fn_b","arguments":""}}
        ]},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[
            {"index":0,"function":{"arguments":"arg_a1"}},
            {"index":1,"function":{"arguments":"arg_b1"}}
        ]},"finish_reason":null}]}"#,
        r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
    ];

    let mut all = Vec::new();
    for chunk in &chunks {
        all.extend(parser.parse_chunk("message", chunk).unwrap());
    }

    assert_eq!(all.len(), 7);
    assert_eq!(
        all[0],
        StreamEvent::ToolCallStart {
            id: "call_a".into(),
            name: "fn_a".into()
        }
    );
    assert_eq!(
        all[1],
        StreamEvent::ToolCallStart {
            id: "call_b".into(),
            name: "fn_b".into()
        }
    );
    assert_eq!(
        all[2],
        StreamEvent::ToolCallDelta {
            id: "call_a".into(),
            arguments_delta: "arg_a1".into()
        }
    );
    assert_eq!(
        all[3],
        StreamEvent::ToolCallDelta {
            id: "call_b".into(),
            arguments_delta: "arg_b1".into()
        }
    );
    assert_eq!(
        all[4],
        StreamEvent::ToolCallEnd {
            id: "call_a".into()
        }
    );
    assert_eq!(
        all[5],
        StreamEvent::ToolCallEnd {
            id: "call_b".into()
        }
    );
    assert!(matches!(all[6], StreamEvent::Done { .. }));
}

#[test]
fn test_reasoning_content() {
    let mut parser = OpenAiSseParser::new();
    let chunk = r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":null,"reasoning_content":"Let me think about this step by step.\n\nFirst, I need to understand the problem."},"finish_reason":null}]}"#;
    let events = parser.parse_chunk("message", chunk).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0],
        StreamEvent::Reasoning {
            content:
                "Let me think about this step by step.\n\nFirst, I need to understand the problem."
                    .into()
        }
    );
}

#[test]
fn test_reasoning_content_empty_skipped() {
    let mut parser = OpenAiSseParser::new();
    let chunk = r#"{"choices":[{"index":0,"delta":{"content":null,"reasoning_content":""},"finish_reason":null}]}"#;
    let events = parser.parse_chunk("message", chunk).unwrap();
    assert!(events.is_empty());
}

#[test]
fn test_reasoning_with_content() {
    let mut parser = OpenAiSseParser::new();
    let chunk = r#"{"choices":[{"index":0,"delta":{"reasoning_content":"Hmm, interesting question.","content":"The answer is 42."},"finish_reason":null}]}"#;
    let events = parser.parse_chunk("message", chunk).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[0],
        StreamEvent::Reasoning {
            content: "Hmm, interesting question.".into()
        }
    );
    assert_eq!(
        events[1],
        StreamEvent::Token {
            content: "The answer is 42.".into()
        }
    );
}
