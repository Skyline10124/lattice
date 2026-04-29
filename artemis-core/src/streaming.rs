//! SSE (Server-Sent Events) streaming infrastructure for LLM providers.
//!
//! This module provides the core types and machinery for parsing streaming
//! responses from LLM providers that use SSE (OpenAI, Anthropic, etc.):
//!
//! - [`StreamEvent`] — unified event enum covering tokens, tool calls, done, errors
//! - [`TokenUsage`] — token count statistics returned in final chunks
//! - [`SseParser`] trait — pluggable parsing strategy per provider
//! - [`OpenAiSseParser`] — parses the OpenAI `chat.completion.chunk` format
//! - [`AnthropicSseParser`] — parses the Anthropic message-streaming format
//! - [`SseStream`] — async `next_event()` interface over [`reqwest_eventsource::EventSource`]
//! - [`EventStream`] — boxed [`Stream`] for ergonomic use with stream combinators
//! - [`parse_raw_sse`] — fallback synchronous parser for raw SSE text

use std::collections::HashMap;
use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::{Stream, StreamExt};
use reqwest_eventsource::{self as re, Event, EventSource};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Errors that can occur during SSE streaming.
#[derive(Debug, thiserror::Error)]
pub enum SseError {
    /// A generic parse error (e.g. unexpected format).
    #[error("SSE parse error: {0}")]
    Parse(String),

    /// An error from the underlying [`reqwest_eventsource`] stream.
    #[error(transparent)]
    ReqwestEventsource(#[from] re::Error),

    /// An error from JSON deserialisation of a chunk payload.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// Token usage statistics returned by the provider in the final stream chunk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// A single event yielded by an LLM provider's streaming SSE response.
///
/// These are the building blocks that callers process to assemble a complete
/// response: accumulate [`Token`] variants into the content string, track
/// tool-call lifecycle via [`ToolCallStart`] / [`ToolCallDelta`] / [`ToolCallEnd`],
/// and finish processing when [`Done`] arrives.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum StreamEvent {
    /// A chunk of generated text content.
    Token { content: String },
    /// A tool call has been requested — contains the tool id and name.
    /// Subsequent argument fragments arrive via [`ToolCallDelta`].
    ToolCallStart { id: String, name: String },
    /// A partial fragment of a tool call's JSON arguments.
    ToolCallDelta { id: String, arguments_delta: String },
    /// Signals that a tool call's argument stream is complete.
    ToolCallEnd { id: String },
    /// The stream is finished.
    Done {
        finish_reason: String,
        usage: Option<TokenUsage>,
    },
    /// A non-fatal error encountered during streaming
    /// (e.g. an API error chunk).
    Error { message: String },
}

/// Parses a single SSE message (event type + data payload) into zero or more
/// [`StreamEvent`]s.
///
/// Implementations are expected to be **stateful** when they need to track
/// tool-call indentities across chunks (both the OpenAI and Anthropic formats
/// omit the tool-call id from delta chunks).
pub trait SseParser: Send + Sync {
    /// Parse one SSE message and return the resulting [`StreamEvent`]s.
    ///
    /// `event_type` is the SSE event field (e.g. `"message"`, `"content_block_delta"`).
    /// `data` is the raw data payload (after stripping `data: `).
    fn parse_chunk(
        &mut self,
        event_type: &str,
        data: &str,
    ) -> Result<Vec<StreamEvent>, Box<dyn std::error::Error>>;
}

/// Parser for the OpenAI `chat.completion.chunk` SSE format.
///
/// Handles:
/// - `data: [DONE]` sentinel → [`StreamEvent::Done`]
/// - Content delta chunks → [`StreamEvent::Token`]
/// - Tool-call delta chunks → [`StreamEvent::ToolCallStart`] / [`StreamEvent::ToolCallDelta`]
/// - Finish-reason chunks → [`StreamEvent::ToolCallEnd`] + [`StreamEvent::Done`]
/// - API error chunks → [`StreamEvent::Error`]
///
/// ## State
///
/// Tracks tool-call ids per `tool_calls[i].index` because OpenAI omits the `id`
/// field from subsequent delta chunks after the first one.
#[derive(Default)]
pub struct OpenAiSseParser {
    tool_call_ids: HashMap<u32, String>,
}

impl OpenAiSseParser {
    pub fn new() -> Self {
        Self {
            tool_call_ids: HashMap::new(),
        }
    }
}

impl SseParser for OpenAiSseParser {
    fn parse_chunk(
        &mut self,
        _event_type: &str,
        data: &str,
    ) -> Result<Vec<StreamEvent>, Box<dyn std::error::Error>> {
        let trimmed = data.trim();
        if trimmed == "[DONE]" {
            return Ok(vec![StreamEvent::Done {
                finish_reason: "stop".into(),
                usage: None,
            }]);
        }

        let root: Value = serde_json::from_str(trimmed)?;

        if let Some(error) = root.get("error") {
            let msg = error["message"]
                .as_str()
                .unwrap_or("Unknown API error")
                .to_string();
            return Ok(vec![StreamEvent::Error { message: msg }]);
        }

        let mut events = Vec::new();

        if let Some(choices) = root["choices"].as_array() {
            for choice in choices {
                let delta = &choice["delta"];
                let finish_reason = choice["finish_reason"].as_str();

                if let Some(content) = delta["content"].as_str() {
                    if !content.is_empty() {
                        events.push(StreamEvent::Token {
                            content: content.to_string(),
                        });
                    }
                }

                if let Some(tool_calls) = delta["tool_calls"].as_array() {
                    for tc in tool_calls {
                        let idx = tc["index"].as_u64().unwrap_or(0) as u32;

                        if let Some(id) = tc["id"].as_str() {
                            let name = tc["function"]["name"].as_str().unwrap_or("");
                            self.tool_call_ids.insert(idx, id.to_string());
                            events.push(StreamEvent::ToolCallStart {
                                id: id.to_string(),
                                name: name.to_string(),
                            });
                        }

                        if let Some(args) = tc["function"]["arguments"].as_str() {
                            if !args.is_empty() {
                                if let Some(id) = self.tool_call_ids.get(&idx) {
                                    events.push(StreamEvent::ToolCallDelta {
                                        id: id.clone(),
                                        arguments_delta: args.to_string(),
                                    });
                                }
                            }
                        }
                    }
                }

                if let Some(reason) = finish_reason {
                    if !reason.is_empty() {
                        for id in self.tool_call_ids.drain().map(|(_, id)| id) {
                            events.push(StreamEvent::ToolCallEnd { id });
                        }

                        let usage = root["usage"].as_object().map(|u| TokenUsage {
                            prompt_tokens: u["prompt_tokens"].as_u64().unwrap_or(0) as u32,
                            completion_tokens: u["completion_tokens"].as_u64().unwrap_or(0) as u32,
                            total_tokens: u["total_tokens"].as_u64().unwrap_or(0) as u32,
                        });

                        events.push(StreamEvent::Done {
                            finish_reason: reason.to_string(),
                            usage,
                        });
                    }
                }
            }
        }

        Ok(events)
    }
}

/// Parser for the Anthropic message-streaming SSE format.
///
/// Anthropic uses named SSE events (`message_start`, `content_block_delta`, …)
/// rather than the OpenAI-style all-in-one JSON chunks.  This parser maps each
/// event type to the corresponding [`StreamEvent`] variant.
///
/// ## Event mapping
///
/// | SSE event              | StreamEvent(s)                                  |
/// |------------------------|-------------------------------------------------|
/// | `message_start`        | _(ignored — metadata)_                          |
/// | `content_block_start`  | `ToolCallStart` (if `type = "tool_use"`)        |
/// | `content_block_delta`  | `Token` / `ToolCallDelta`                       |
/// | `content_block_stop`   | `ToolCallEnd` (if it was a tool_use block)      |
/// | `message_delta`        | `Done`                                          |
/// | `message_stop`         | _(ignored — redundant with `message_delta`)_    |
/// | `ping`                 | _(ignored — keep-alive)_                        |
#[derive(Default)]
pub struct AnthropicSseParser {
    tool_call_ids: HashMap<u32, String>,
    input_tokens: u32,
}

impl AnthropicSseParser {
    pub fn new() -> Self {
        Self {
            tool_call_ids: HashMap::new(),
            input_tokens: 0,
        }
    }
}

impl SseParser for AnthropicSseParser {
    fn parse_chunk(
        &mut self,
        event_type: &str,
        data: &str,
    ) -> Result<Vec<StreamEvent>, Box<dyn std::error::Error>> {
        if data.trim().is_empty() {
            return Ok(vec![]);
        }

        let root: Value = serde_json::from_str(data)?;

        match event_type {
            "message_start" => {
                if let Some(msg) = root.get("message") {
                    self.input_tokens = msg["usage"]["input_tokens"].as_u64().unwrap_or(0) as u32;
                }
                Ok(vec![])
            }
            "content_block_start" => {
                let idx = root["index"].as_u64().unwrap_or(0) as u32;
                let block = &root["content_block"];
                match block["type"].as_str() {
                    Some("tool_use") => {
                        let id = block["id"].as_str().unwrap_or("").to_string();
                        let name = block["name"].as_str().unwrap_or("").to_string();
                        self.tool_call_ids.insert(idx, id.clone());
                        Ok(vec![StreamEvent::ToolCallStart { id, name }])
                    }
                    _ => Ok(vec![]),
                }
            }
            "content_block_delta" => {
                let idx = root["index"].as_u64().unwrap_or(0) as u32;
                let delta = &root["delta"];
                match delta["type"].as_str() {
                    Some("text_delta") => {
                        let text = delta["text"].as_str().unwrap_or("");
                        if text.is_empty() {
                            Ok(vec![])
                        } else {
                            Ok(vec![StreamEvent::Token {
                                content: text.to_string(),
                            }])
                        }
                    }
                    Some("input_json_delta") => {
                        let partial = delta["partial_json"].as_str().unwrap_or("");
                        if partial.is_empty() {
                            Ok(vec![])
                        } else if let Some(id) = self.tool_call_ids.get(&idx).cloned() {
                            Ok(vec![StreamEvent::ToolCallDelta {
                                id,
                                arguments_delta: partial.to_string(),
                            }])
                        } else {
                            Ok(vec![])
                        }
                    }
                    _ => Ok(vec![]),
                }
            }
            "content_block_stop" => {
                let idx = root["index"].as_u64().unwrap_or(0) as u32;
                if let Some(id) = self.tool_call_ids.remove(&idx) {
                    Ok(vec![StreamEvent::ToolCallEnd { id }])
                } else {
                    Ok(vec![])
                }
            }
            "message_delta" => {
                let stop_reason = root["delta"]["stop_reason"].as_str().unwrap_or("end_turn");
                let output_tokens = root["usage"]["output_tokens"].as_u64().unwrap_or(0) as u32;
                let total_tokens = self.input_tokens + output_tokens;
                let usage = root["usage"].as_object().map(|_| TokenUsage {
                    prompt_tokens: self.input_tokens,
                    completion_tokens: output_tokens,
                    total_tokens,
                });
                Ok(vec![StreamEvent::Done {
                    finish_reason: stop_reason.to_string(),
                    usage,
                }])
            }
            "message_stop" | "ping" => Ok(vec![]),
            "error" => {
                let msg = root["error"]["message"]
                    .as_str()
                    .or_else(|| root["error"]["type"].as_str())
                    .unwrap_or("Unknown Anthropic streaming error")
                    .to_string();
                Ok(vec![StreamEvent::Error { message: msg }])
            }
            _ => Ok(vec![]),
        }
    }
}

/// An SSE stream that wraps a [`reqwest_eventsource::EventSource`] and parses
/// events using a pluggable [`SseParser`].
///
/// Provides a simple async `next_event()` method for callers that want to
/// process events one at a time without pulling in the [`Stream`] machinery.
///
/// # Example
///
/// ```rust,ignore
/// use reqwest_eventsource::RequestBuilderExt;
///
/// let client = reqwest::Client::new();
/// let es = client
///     .get("https://api.openai.com/v1/chat/completions")
///     .header("Authorization", "Bearer sk-...")
///     .eventsource()?;
///
/// let mut stream = SseStream::new(es, OpenAiSseParser::new());
/// while let Some(event) = stream.next_event().await? {
///     match event {
///         StreamEvent::Token { content } => print!("{content}"),
///         StreamEvent::Done { .. } => break,
///         _ => {}
///     }
/// }
/// ```
pub struct SseStream<Parser: SseParser> {
    event_source: Pin<Box<EventSource>>,
    parser: Parser,
    buffer: VecDeque<StreamEvent>,
}

impl<Parser: SseParser> SseStream<Parser> {
    /// Create a new stream from an [`EventSource`] and a parser.
    pub fn new(event_source: EventSource, parser: Parser) -> Self {
        Self {
            event_source: Box::pin(event_source),
            parser,
            buffer: VecDeque::new(),
        }
    }

    /// Read and parse the next SSE event, returning the resulting
    /// [`StreamEvent`].
    ///
    /// Returns `Ok(None)` when the underlying event source has been exhausted
    /// (normal stream end).
    pub async fn next_event(&mut self) -> Result<Option<StreamEvent>, SseError> {
        loop {
            // Drain buffered events first
            if let Some(event) = self.buffer.pop_front() {
                return Ok(Some(event));
            }

            match self.event_source.next().await {
                Some(Ok(Event::Open)) => continue,
                Some(Ok(Event::Message(msg))) => {
                    let events = self
                        .parser
                        .parse_chunk(&msg.event, &msg.data)
                        .map_err(|e| SseError::Parse(e.to_string()))?;
                    self.buffer.extend(events);
                }
                Some(Err(e)) => return Err(SseError::from(e)),
                None => return Ok(None),
            }
        }
    }

    /// Get a reference to the underlying parser (e.g. to access accumulated
    /// state after the stream finishes).
    pub fn parser(&self) -> &Parser {
        &self.parser
    }

    /// Consume the stream and return the parser.
    pub fn into_parser(self) -> Parser {
        self.parser
    }
}

/// A boxed, pinned SSE event stream that implements [`futures::Stream`].
///
/// Construct it from an [`EventSource`] + [`SseParser`].
///
/// # Stream item type
///
/// `Item = StreamEvent` — network / parse errors are surfaced through the
/// [`StreamEvent::Error`] variant.  Stream termination (normal or error)
/// is signalled via `Poll::Ready(None)`.
pub struct EventStream {
    event_source: Pin<Box<EventSource>>,
    parser: Box<dyn SseParser>,
    buffer: VecDeque<StreamEvent>,
}

impl EventStream {
    /// Create a new event stream from an [`EventSource`] and a boxed parser.
    pub fn new(event_source: EventSource, parser: Box<dyn SseParser>) -> Self {
        Self {
            event_source: Box::pin(event_source),
            parser,
            buffer: VecDeque::new(),
        }
    }
}

impl Stream for EventStream {
    type Item = StreamEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        if let Some(event) = this.buffer.pop_front() {
            return Poll::Ready(Some(event));
        }

        loop {
            match this.event_source.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(Event::Open))) => continue,
                Poll::Ready(Some(Ok(Event::Message(msg)))) => {
                    match this.parser.parse_chunk(&msg.event, &msg.data) {
                        Ok(events) => {
                            if events.is_empty() {
                                continue;
                            }
                            let mut iter = events.into_iter();
                            // Return first event, buffer the rest
                            let first = iter.next().expect("checked !is_empty()");
                            this.buffer.extend(iter);
                            return Poll::Ready(Some(first));
                        }
                        Err(e) => {
                            return Poll::Ready(Some(StreamEvent::Error {
                                message: e.to_string(),
                            }));
                        }
                    }
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(StreamEvent::Error {
                        message: e.to_string(),
                    }));
                }
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// A raw SSE event produced by [`parse_raw_sse`].
#[derive(Debug, Clone, PartialEq)]
pub struct RawSseEvent {
    pub event: String,
    pub data: String,
    pub id: Option<String>,
}

/// Parse raw SSE text into a sequence of [`RawSseEvent`]s.
///
/// This is a **synchronous** fallback for environments where the
/// `reqwest-eventsource` / `eventsource-stream` async machinery is not
/// available (e.g. tests, file-based processing).
///
/// # SSE wire format
///
/// ```text
/// event: <type>
/// data: <payload line 1>
/// data: <payload line 2>
/// <blank line = event delimiter>
/// ```
///
/// Multiple `data:` lines are joined with `'\n'`. Events are separated by
/// blank lines.
pub fn parse_raw_sse(input: &str) -> Vec<RawSseEvent> {
    let mut events = Vec::new();
    let mut current_event = String::new();
    let mut current_data = String::new();
    let mut current_id: Option<String> = None;

    for line in input.lines() {
        if line.trim().is_empty() {
            // Blank line → end of current event
            if !current_event.is_empty() || !current_data.is_empty() {
                events.push(RawSseEvent {
                    event: std::mem::take(&mut current_event),
                    data: std::mem::take(&mut current_data),
                    id: current_id.take(),
                });
            }
            // Also reset current_event even if empty (e.g. event with only data:)
            current_event.clear();
            current_data.clear();
        } else if let Some(value) = line.strip_prefix("event:") {
            current_event = value.trim().to_string();
        } else if let Some(value) = line.strip_prefix("data:") {
            if !current_data.is_empty() {
                current_data.push('\n');
            }
            current_data.push_str(value.trim());
        } else if let Some(value) = line.strip_prefix("id:") {
            current_id = Some(value.trim().to_string());
        }
        // `retry:` lines are silently ignored.
    }

    // Handle trailing event (no trailing blank line)
    if !current_event.is_empty() || !current_data.is_empty() {
        events.push(RawSseEvent {
            event: current_event,
            data: current_data,
            id: current_id,
        });
    }

    events
}

/// Convenience function: parse raw SSE text through a parser and collect all
/// resulting [`StreamEvent`]s.
///
/// Useful for **testing** parser implementations without an HTTP connection.
pub fn parse_sse_text(
    input: &str,
    parser: &mut dyn SseParser,
) -> Result<Vec<StreamEvent>, Box<dyn std::error::Error>> {
    let mut all = Vec::new();
    for raw in parse_raw_sse(input) {
        let events = parser.parse_chunk(&raw.event, &raw.data)?;
        all.extend(events);
    }
    Ok(all)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_openai_done_sentinel() {
        let mut parser = OpenAiSseParser::new();
        let events = parser.parse_chunk("message", "[DONE]").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::Done {
                finish_reason,
                usage,
            } => {
                assert_eq!(finish_reason, "stop");
                assert!(usage.is_none());
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[test]
    fn test_openai_content_chunk() {
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
    fn test_openai_multiple_content_chunks() {
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
    fn test_openai_empty_delta_skipped() {
        let mut parser = OpenAiSseParser::new();
        // A chunk with no content and no finish_reason (empty delta)
        let chunk = r#"{"choices":[{"index":0,"delta":{},"finish_reason":null}]}"#;
        let events = parser.parse_chunk("message", chunk).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn test_openai_tool_call_streaming() {
        let mut parser = OpenAiSseParser::new();
        let chunks = vec![
            // First chunk: tool call declaration with id + name
            r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":null,"tool_calls":[{"index":0,"id":"call_abc123","type":"function","function":{"name":"get_weather","arguments":""}}]},"finish_reason":null}]}"#,
            // Delta chunks: arguments fragments (no id, no name)
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"location\":\"San"}}]},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":" Francisco\"}"}}]},"finish_reason":null}]}"#,
            // Final chunk: finish_reason = tool_calls
            r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        ];

        let mut all_events = Vec::new();
        for chunk in &chunks {
            let events = parser.parse_chunk("message", chunk).unwrap();
            all_events.extend(events);
        }

        assert_eq!(all_events.len(), 5);

        // 1. ToolCallStart
        assert_eq!(
            all_events[0],
            StreamEvent::ToolCallStart {
                id: "call_abc123".into(),
                name: "get_weather".into(),
            }
        );

        // 2-3. ToolCallDelta
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

        // 4. ToolCallEnd
        assert_eq!(
            all_events[3],
            StreamEvent::ToolCallEnd {
                id: "call_abc123".into()
            }
        );

        // 5. Done
        assert_eq!(
            all_events[4],
            StreamEvent::Done {
                finish_reason: "tool_calls".into(),
                usage: None,
            }
        );
    }

    #[test]
    fn test_openai_api_error() {
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
    fn test_openai_done_with_usage() {
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
    fn test_anthropic_text_streaming() {
        let mut parser = AnthropicSseParser::new();

        // message_start (ignored)
        let events = parser
            .parse_chunk(
                "message_start",
                r#"{"type":"message_start","message":{"id":"msg_1","content":[],"model":"claude-3-5-sonnet","role":"assistant","stop_reason":null,"usage":{"input_tokens":10,"output_tokens":1}}}"#,
            )
            .unwrap();
        assert!(events.is_empty());

        // content_block_start (text)
        let events = parser
            .parse_chunk(
                "content_block_start",
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            )
            .unwrap();
        assert!(events.is_empty());

        // content_block_delta (text_delta)
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

        // content_block_delta (text_delta, continuation)
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

        // content_block_stop (text → no ToolCallEnd)
        let events = parser
            .parse_chunk(
                "content_block_stop",
                r#"{"type":"content_block_stop","index":0}"#,
            )
            .unwrap();
        assert!(events.is_empty());

        // message_delta → Done
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
                assert_eq!(finish_reason, "end_turn");
                let usage = usage.as_ref().expect("expected usage");
                assert_eq!(usage.prompt_tokens, 10, "input_tokens from message_start");
                assert_eq!(usage.completion_tokens, 50);
                assert_eq!(usage.total_tokens, 60, "total = input + output");
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[test]
    fn test_anthropic_tool_call_streaming() {
        let mut parser = AnthropicSseParser::new();

        // content_block_start (tool_use)
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

        // content_block_delta (input_json_delta)
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

        // content_block_stop → ToolCallEnd
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
    fn test_anthropic_ping_ignored() {
        let mut parser = AnthropicSseParser::new();
        let events = parser.parse_chunk("ping", "{}").unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn test_anthropic_error_event() {
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
    fn test_anthropic_usage_tracks_input_tokens() {
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
    fn test_anthropic_message_stop_ignored() {
        let mut parser = AnthropicSseParser::new();
        let events = parser
            .parse_chunk("message_stop", r#"{"type":"message_stop"}"#)
            .unwrap();
        assert!(events.is_empty());
    }

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
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], StreamEvent::Token { .. }));
        assert!(matches!(events[1], StreamEvent::Token { .. }));
        assert!(matches!(events[2], StreamEvent::Done { .. }));
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
    fn test_openai_whitespace_done() {
        let mut parser = OpenAiSseParser::new();
        let events = parser.parse_chunk("message", "  [DONE]  ").unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], StreamEvent::Done { .. }));
    }

    #[test]
    fn test_anthropic_empty_data_is_noop() {
        let mut parser = AnthropicSseParser::new();
        let events = parser.parse_chunk("ping", "").unwrap();
        assert!(events.is_empty());
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
            // Two tool calls declared in the same chunk
            r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":null,"tool_calls":[
                {"index":0,"id":"call_a","type":"function","function":{"name":"fn_a","arguments":""}},
                {"index":1,"id":"call_b","type":"function","function":{"name":"fn_b","arguments":""}}
            ]},"finish_reason":null}]}"#,
            // Both get argument deltas
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[
                {"index":0,"function":{"arguments":"arg_a1"}},
                {"index":1,"function":{"arguments":"arg_b1"}}
            ]},"finish_reason":null}]}"#,
            // Finish
            r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        ];

        let mut all = Vec::new();
        for chunk in &chunks {
            all.extend(parser.parse_chunk("message", chunk).unwrap());
        }

        assert_eq!(all.len(), 7);
        // Start a, start b, delta a, delta b, end a, end b, done
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
        // Ends can be in any order due to HashMap iteration
        let ends: Vec<_> = all[4..6]
            .iter()
            .map(|e| match e {
                StreamEvent::ToolCallEnd { id } => id.as_str(),
                _ => panic!("expected ToolCallEnd"),
            })
            .collect();
        assert!(ends.contains(&"call_a"));
        assert!(ends.contains(&"call_b"));
        assert!(matches!(all[6], StreamEvent::Done { .. }));
    }
}
