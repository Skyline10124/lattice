use crate::agent_loop::LoopEvent;

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
