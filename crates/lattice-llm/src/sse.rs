//! Bounded SSE (Server-Sent Events) framing for provider stream parsers.
//!
//! Provider modules share this parser to safely consume streaming HTTP
//! responses without unbounded memory growth.

/// Maximum number of SSE events processed per stream.
pub const MAX_SSE_EVENTS: usize = 100_000;

/// Maximum size in bytes of a single SSE `data:` field value.
pub const MAX_SSE_DATA_SIZE: usize = 256 * 1024;

/// Maximum total buffer size across all in-flight SSE fields.
pub const MAX_SSE_BUFFER_SIZE: usize = 2 * 1024 * 1024;

/// A raw SSE event before provider-specific interpretation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawSseEvent {
    pub event_type: Option<String>,
    pub data: String,
    pub id: Option<String>,
}

/// Parse a buffer of SSE text into zero or more [`RawSseEvent`] values.
///
/// Enforces `MAX_SSE_EVENTS`, `MAX_SSE_DATA_SIZE`, and `MAX_SSE_BUFFER_SIZE`.
/// Returns an error when limits are exceeded or the input is malformed.
pub fn parse_raw_sse(input: &str) -> Result<Vec<RawSseEvent>, String> {
    let mut events: Vec<RawSseEvent> = Vec::new();
    let mut current_event_type: Option<String> = None;
    let mut current_data = String::new();
    let mut current_id: Option<String> = None;
    let mut total_buffer = 0usize;

    for line in input.lines() {
        let line = line.trim_end_matches('\r');

        total_buffer += line.len();
        if total_buffer > MAX_SSE_BUFFER_SIZE {
            return Err(format!(
                "SSE buffer size exceeded: {} > {}",
                total_buffer, MAX_SSE_BUFFER_SIZE
            ));
        }

        if line.is_empty() {
            // Empty line dispatches the current event
            if !current_data.is_empty() || current_event_type.is_some() {
                if events.len() >= MAX_SSE_EVENTS {
                    return Err(format!("SSE event count exceeded: {}", MAX_SSE_EVENTS));
                }
                events.push(RawSseEvent {
                    event_type: current_event_type.take(),
                    data: std::mem::take(&mut current_data),
                    id: current_id.take(),
                });
            }
            continue;
        }

        if line.starts_with(':') {
            // SSE comment — skip
            continue;
        }

        if let Some(value) = line.strip_prefix("event:") {
            current_event_type = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            let value = value.strip_prefix(' ').unwrap_or(value);
            if current_data.len() + value.len() > MAX_SSE_DATA_SIZE {
                return Err(format!(
                    "SSE data field size exceeded: {} > {}",
                    current_data.len() + value.len(),
                    MAX_SSE_DATA_SIZE
                ));
            }
            if !current_data.is_empty() {
                current_data.push('\n');
            }
            current_data.push_str(value);
        } else if let Some(value) = line.strip_prefix("id:") {
            current_id = Some(value.trim().to_string());
        }
        // Unknown fields are ignored per SSE spec
    }

    // Flush trailing event if any data was accumulated
    if !current_data.is_empty() || current_event_type.is_some() {
        if events.len() >= MAX_SSE_EVENTS {
            return Err(format!("SSE event count exceeded: {}", MAX_SSE_EVENTS));
        }
        events.push(RawSseEvent {
            event_type: current_event_type.take(),
            data: if current_data.is_empty() {
                String::new()
            } else {
                std::mem::take(&mut current_data)
            },
            id: current_id.take(),
        });
    }

    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_event() {
        let input = "data: hello\n\n";
        let events = parse_raw_sse(input).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "hello");
        assert_eq!(events[0].event_type, None);
    }

    #[test]
    fn parse_multi_line_data() {
        let input = "data: line1\ndata: line2\n\n";
        let events = parse_raw_sse(input).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "line1\nline2");
    }

    #[test]
    fn parse_event_with_type() {
        let input = "event: message\ndata: hello\n\n";
        let events = parse_raw_sse(input).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type.as_deref(), Some("message"));
        assert_eq!(events[0].data, "hello");
    }

    #[test]
    fn skip_comments() {
        let input = ": this is a comment\ndata: hello\n\n";
        let events = parse_raw_sse(input).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "hello");
    }

    #[test]
    fn done_marker_is_just_data() {
        // "[DONE]" is provider-specific — the raw parser just sees a data field
        let input = "data: [DONE]\n\n";
        let events = parse_raw_sse(input).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "[DONE]");
    }

    #[test]
    fn reject_data_size_exceeded() {
        let big = "x".repeat(MAX_SSE_DATA_SIZE + 1);
        let input = format!("data: {}\n\n", big);
        assert!(parse_raw_sse(&input).is_err());
    }

    #[test]
    fn reject_event_count_exceeded() {
        // Generate events approaching the limit, then one more
        let mut input = String::new();
        for i in 0..MAX_SSE_EVENTS {
            input.push_str(&format!("data: evt{}\n\n", i));
        }
        // This should just barely succeed
        let events = parse_raw_sse(&input).unwrap();
        assert_eq!(events.len(), MAX_SSE_EVENTS);

        // One more should fail
        input.push_str("data: one-too-many\n\n");
        assert!(parse_raw_sse(&input).is_err());
    }

    #[test]
    fn parse_event_with_id() {
        let input = "id: 42\ndata: hello\n\n";
        let events = parse_raw_sse(input).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id.as_deref(), Some("42"));
        assert_eq!(events[0].data, "hello");
    }

    #[test]
    fn empty_input_yields_empty() {
        let events = parse_raw_sse("").unwrap();
        assert!(events.is_empty());
    }
}
