use crate::agent_loop::LoopEvent;
use crate::engine::{Event, ToolCallInfo};
use pyo3::prelude::*;

#[pyclass]
pub struct StreamIterator {
    pub events: Vec<LoopEvent>,
    position: usize,
}

impl StreamIterator {
    pub fn new(events: Vec<LoopEvent>) -> Self {
        StreamIterator {
            events,
            position: 0,
        }
    }
}

fn loop_event_to_event(le: &LoopEvent) -> Event {
    match le {
        LoopEvent::Token { content } => Event {
            kind: "token".to_string(),
            content: Some(content.clone()),
            tool_calls: None,
            finish_reason: None,
        },
        LoopEvent::ToolCallRequired { tool_calls } => Event {
            kind: "tool_call_required".to_string(),
            content: None,
            tool_calls: Some(tool_calls.iter().map(ToolCallInfo::from).collect()),
            finish_reason: None,
        },
        LoopEvent::Done {
            finish_reason,
            final_message,
        } => Event {
            kind: "done".to_string(),
            content: Some(final_message.content.clone()),
            tool_calls: final_message
                .tool_calls
                .as_ref()
                .map(|tcs| tcs.iter().map(ToolCallInfo::from).collect()),
            finish_reason: Some(finish_reason.clone()),
        },
        LoopEvent::Error { message } => Event {
            kind: "error".to_string(),
            content: Some(message.clone()),
            tool_calls: None,
            finish_reason: None,
        },
        LoopEvent::Interrupted => Event {
            kind: "interrupted".to_string(),
            content: None,
            tool_calls: None,
            finish_reason: None,
        },
    }
}

#[pymethods]
impl StreamIterator {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__(mut slf: PyRefMut<'_, Self>) -> Option<Event> {
        if slf.position < slf.events.len() {
            let event = loop_event_to_event(&slf.events[slf.position]);
            slf.position += 1;
            Some(event)
        } else {
            None
        }
    }
}

impl Iterator for StreamIterator {
    type Item = LoopEvent;

    fn next(&mut self) -> Option<Self::Item> {
        if self.position < self.events.len() {
            let ev = self.events[self.position].clone();
            self.position += 1;
            Some(ev)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_loop::LoopEvent;

    #[test]
    fn test_stream_iterator_yields_all_events() {
        let events = vec![
            LoopEvent::Token {
                content: "hello".into(),
            },
            LoopEvent::Done {
                finish_reason: "stop".into(),
                final_message: crate::types::Message {
                    role: crate::types::Role::Assistant,
                    content: "hello".into(),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
            },
        ];
        let iter = StreamIterator::new(events.clone());
        let collected: Vec<LoopEvent> = iter.collect();
        assert_eq!(collected.len(), 2);
    }

    #[test]
    fn test_stream_iterator_empty() {
        let iter = StreamIterator::new(vec![]);
        let collected: Vec<LoopEvent> = iter.collect();
        assert!(collected.is_empty());
    }

    #[test]
    fn test_stream_iterator_single() {
        let events = vec![LoopEvent::Token {
            content: "hello".into(),
        }];
        let mut iter = StreamIterator::new(events);
        assert!(iter.next().is_some());
        assert!(iter.next().is_none());
    }
}
