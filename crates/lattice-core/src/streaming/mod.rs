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
//! - [`GeminiSseParser`] — parses the Gemini `streamGenerateContent` format
//! - [`parse_raw_sse`] — synchronous parser for raw SSE text
//! - [`sse_from_bytes_stream`] — async SSE parser from raw HTTP response body

mod anthropic;
mod gemini;
mod openai;

use serde::{Deserialize, Serialize};

pub use anthropic::AnthropicSseParser;
pub use gemini::GeminiSseParser;
pub use openai::OpenAiSseParser;

/// Maximum number of SSE events to parse from a single input.
const MAX_SSE_EVENTS: usize = 10000;
/// Maximum size (bytes) of a single SSE data field.
const MAX_SSE_DATA_SIZE: usize = 1_000_000;
/// Maximum size (bytes) of the SSE buffer before forcing a parse attempt.
const MAX_SSE_BUFFER_SIZE: usize = 10_000_000;

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
    /// A chunk of reasoning/thinking content (e.g., DeepSeek R1/V4 thinking chain,
    /// Anthropic thinking_delta, Gemini `thought: true` parts).
    Reasoning { content: String },
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

/// A raw SSE event produced by [`parse_raw_sse`].
#[derive(Debug, Clone, PartialEq)]
pub struct RawSseEvent {
    pub event: String,
    pub data: String,
    pub id: Option<String>,
}

/// Parse raw SSE text into a sequence of [`RawSseEvent`]s.
///
/// This is a **synchronous** fallback for environments where async streaming
/// is not available (e.g. tests, file-based processing).
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
                if events.len() >= MAX_SSE_EVENTS {
                    tracing::warn!(
                        "SSE event count limit ({MAX_SSE_EVENTS}) reached, dropping remaining events"
                    );
                    break;
                }
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
            let trimmed = value.trim();
            if current_data.len().saturating_add(trimmed.len()) > MAX_SSE_DATA_SIZE {
                tracing::warn!(
                    "SSE data field size limit ({MAX_SSE_DATA_SIZE}) exceeded, clearing current event data"
                );
                current_event.clear();
                current_data.clear();
                continue;
            }
            if !current_data.is_empty() {
                current_data.push('\n');
            }
            current_data.push_str(trimmed);
        } else if let Some(value) = line.strip_prefix("id:") {
            current_id = Some(value.trim().to_string());
        }
        // `retry:` lines are silently ignored.
    }

    // Handle trailing event (no trailing blank line)
    if (!current_event.is_empty() || !current_data.is_empty()) && events.len() < MAX_SSE_EVENTS {
        events.push(RawSseEvent {
            event: current_event,
            data: current_data,
            id: current_id,
        });
    }

    events
}

/// Parse a stream of byte chunks (from a raw HTTP response body) into SSE events.
///
/// On end-of-stream, any residual data in the buffer that does not end with
/// an empty-line delimiter is flushed and parsed as a final SSE event.
pub fn sse_from_bytes_stream(
    body: impl futures::Stream<Item = Result<impl AsRef<[u8]>, impl std::fmt::Display>> + Send + 'static,
    parser: Box<dyn SseParser>,
) -> impl futures::Stream<Item = StreamEvent> + Send {
    use futures::StreamExt;

    // Chain a synthetic `None` sentinel after the real stream so we can
    // flush the residual buffer.
    let body = body.map(Some).chain(futures::stream::once(async { None }));

    let mut parser = parser;
    let mut buf = String::new();

    body.flat_map(move |item| {
        // --- Process incoming chunk ---
        if let Some(ref chunk) = item {
            let text = match chunk {
                Ok(bytes) => String::from_utf8_lossy(bytes.as_ref()).to_string(),
                Err(e) => {
                    return futures::stream::iter(vec![StreamEvent::Error {
                        message: format!("HTTP body stream error: {e}"),
                    }])
                }
            };

            // Normalize CRLF to LF so SSE framing works across all servers
            buf.push_str(&text.replace("\r\n", "\n"));
        }

        // If the buffer exceeds the size limit, force-parse what we have
        // to prevent unbounded memory growth.
        if buf.len() > MAX_SSE_BUFFER_SIZE {
            tracing::warn!(
                "SSE buffer size limit ({MAX_SSE_BUFFER_SIZE}) exceeded, force-parsing buffer"
            );
        }

        // Parse complete SSE events (delimited by blank lines)
        let mut events = Vec::new();

        // Also force-parse when buffer exceeds the limit (no blank-line delimiter found)
        let force_parse = buf.len() > MAX_SSE_BUFFER_SIZE && buf.find("\n\n").is_none();

        if force_parse {
            for raw_event in parse_raw_sse(&buf) {
                match parser.parse_chunk(&raw_event.event, &raw_event.data) {
                    Ok(evts) => events.extend(evts),
                    Err(e) => events.push(StreamEvent::Error {
                        message: format!("SSE parse error: {e}"),
                    }),
                }
            }
            events.push(StreamEvent::Error {
                message: "SSE buffer overflow: force-parsed without proper event delimiter".into(),
            });
            buf.clear();
        } else {
            while let Some(pos) = buf.find("\n\n") {
                let raw = buf[..pos].to_string();
                buf.drain(..pos + 2);

                for raw_event in parse_raw_sse(&raw) {
                    match parser.parse_chunk(&raw_event.event, &raw_event.data) {
                        Ok(evts) => events.extend(evts),
                        Err(e) => events.push(StreamEvent::Error {
                            message: format!("SSE parse error: {e}"),
                        }),
                    }
                }
            }

            // If buffer still exceeds the limit after parsing delimited events,
            // trim it to prevent unbounded growth.
            if buf.len() > MAX_SSE_BUFFER_SIZE {
                tracing::warn!(
                    "SSE buffer still oversized after delimited parse, clearing residual {} bytes",
                    buf.len()
                );
                events.push(StreamEvent::Error {
                    message: "SSE buffer overflow: residual data discarded".into(),
                });
                buf.clear();
            }
        }

        // --- End-of-stream flush: parse residual buffer as final SSE event ---
        if item.is_none() && !buf.is_empty() {
            tracing::debug!(
                "SSE stream ended with {} residual bytes — flushing as final event",
                buf.len()
            );
            for raw_event in parse_raw_sse(&buf) {
                match parser.parse_chunk(&raw_event.event, &raw_event.data) {
                    Ok(evts) => events.extend(evts),
                    Err(e) => events.push(StreamEvent::Error {
                        message: format!("SSE flush parse error: {e}"),
                    }),
                }
            }
            buf.clear();
        }

        futures::stream::iter(events)
    })
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
mod tests;
