#[derive(Debug, Clone, PartialEq)]
pub struct ContextEvent {
    pub topic: String,
    pub source: String,
    pub payload: serde_json::Value,
}

impl ContextEvent {
    pub fn new(
        topic: impl Into<String>,
        source: impl Into<String>,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            topic: topic.into(),
            source: source.into(),
            payload,
        }
    }
}

#[derive(Default)]
pub struct BusEventCollector {
    pending: Vec<ContextEvent>,
}

impl BusEventCollector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, event: ContextEvent) {
        self.pending.push(event);
    }

    pub fn push_many(&mut self, events: Vec<ContextEvent>) {
        self.pending.extend(events);
    }

    pub fn drain(&mut self) -> Vec<ContextEvent> {
        std::mem::take(&mut self.pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_event(topic: &str) -> ContextEvent {
        ContextEvent::new(topic, "test", serde_json::json!({"topic": topic}))
    }

    #[test]
    fn test_push_and_drain() {
        let mut collector = BusEventCollector::new();
        collector.push(make_event("a"));
        collector.push(make_event("b"));
        let events = collector.drain();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].topic, "a");
        assert_eq!(events[1].topic, "b");
        // drain clears
        assert!(collector.drain().is_empty());
    }

    #[test]
    fn test_push_many() {
        let mut collector = BusEventCollector::new();
        collector.push_many(vec![make_event("x"), make_event("y")]);
        let events = collector.drain();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].topic, "x");
    }

    #[test]
    fn test_drain_empty() {
        let mut collector = BusEventCollector::new();
        assert!(collector.drain().is_empty());
    }
}
