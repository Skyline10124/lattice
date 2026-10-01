// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

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
    }

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
