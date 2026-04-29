use artemis_core::types::Message;
use std::collections::HashMap;

/// Trait for cross-session conversation memory.
pub trait Memory: Send + Sync {
    /// Store a message in the given session.
    fn save(&mut self, session: &str, msg: &Message);

    /// Return all messages for a session in chronological order.
    fn history(&self, session: &str) -> Vec<Message>;

    /// Search past sessions for messages relevant to a query.
    fn search(&self, _query: &str, _limit: usize) -> Vec<Message> {
        vec![]
    }
}

/// Default implementation: in-memory HashMap. Not persisted across restarts.
pub struct InMemoryMemory {
    sessions: HashMap<String, Vec<Message>>,
}

impl InMemoryMemory {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
        }
    }
}

impl Default for InMemoryMemory {
    fn default() -> Self {
        Self::new()
    }
}

impl Memory for InMemoryMemory {
    fn save(&mut self, session: &str, msg: &Message) {
        self.sessions
            .entry(session.to_string())
            .or_default()
            .push(msg.clone());
    }

    fn history(&self, session: &str) -> Vec<Message> {
        self.sessions.get(session).cloned().unwrap_or_default()
    }

    fn search(&self, _query: &str, _limit: usize) -> Vec<Message> {
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use artemis_core::types::{Message, Role};

    #[test]
    fn test_save_and_history() {
        let mut mem = InMemoryMemory::new();
        let msg = Message {
            role: Role::User,
            content: "hello".to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        };
        mem.save("session-1", &msg);
        let history = mem.history("session-1");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].content, "hello");
    }

    #[test]
    fn test_history_empty_session() {
        let mem = InMemoryMemory::new();
        assert!(mem.history("nonexistent").is_empty());
    }

    #[test]
    fn test_multiple_messages_order() {
        let mut mem = InMemoryMemory::new();
        for i in 0..3 {
            mem.save(
                "s",
                &Message {
                    role: Role::User,
                    content: format!("msg{}", i),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
            );
        }
        let h = mem.history("s");
        assert_eq!(h.len(), 3);
        assert_eq!(h[0].content, "msg0");
        assert_eq!(h[2].content, "msg2");
    }
}
