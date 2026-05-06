use std::collections::HashMap;

use lattice_core::types::Message;
use lattice_core::types::Role;
use lattice_core::ResolvedModel;

pub struct AgentState {
    pub messages: Vec<Message>,
    pub resolved: ResolvedModel,
    /// Cumulative total tokens used across all turns.
    pub token_usage: u64,
    /// Maps tool_call_id to function_name so push_tool_result can set the
    /// correct `name` field (required by Gemini for functionResponse.name).
    tool_names: HashMap<String, String>,
}

#[derive(Clone)]
pub(crate) struct AgentStateSnapshot {
    messages: Vec<Message>,
    token_usage: u64,
    tool_names: HashMap<String, String>,
}

impl AgentState {
    pub fn new(resolved: ResolvedModel) -> Self {
        Self {
            messages: vec![],
            resolved,
            token_usage: 0,
            tool_names: HashMap::new(),
        }
    }

    pub fn push_system_message(&mut self, content: &str) {
        // Replace the first system message, or append if none exists.
        // This prevents duplicate system messages when the prompt engine
        // recompiles and re-pushes the system section each iteration.
        if let Some(msg) = self.messages.iter_mut().find(|m| m.role == Role::System) {
            msg.content = content.to_string();
        } else {
            self.messages.insert(
                0,
                Message {
                    role: Role::System,
                    content: content.to_string(),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
            );
        }
    }

    pub fn push_user_message(&mut self, content: &str) {
        self.messages.push(Message {
            role: Role::User,
            content: content.to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });
    }

    pub fn seed_messages(&mut self, messages: Vec<Message>) {
        self.tool_names.clear();
        self.messages = messages;
    }

    pub(crate) fn snapshot(&self) -> AgentStateSnapshot {
        AgentStateSnapshot {
            messages: self.messages.clone(),
            token_usage: self.token_usage,
            tool_names: self.tool_names.clone(),
        }
    }

    pub(crate) fn restore(&mut self, snapshot: AgentStateSnapshot) {
        self.messages = snapshot.messages;
        self.token_usage = snapshot.token_usage;
        self.tool_names = snapshot.tool_names;
    }

    /// Replace the first Role::User message in-place.
    /// Used by the prompt engine to refresh compiled context each tool iteration
    /// without growing the message list.
    pub fn replace_first_user_message(&mut self, content: &str) {
        if let Some(msg) = self.messages.iter_mut().find(|m| m.role == Role::User) {
            msg.content = content.to_string();
        } else {
            self.push_user_message(content);
        }
    }

    /// Replace the user message for the active turn without rewriting older
    /// conversation turns. If the active turn has not inserted a user message
    /// yet, append one.
    pub fn upsert_user_message_from(&mut self, start: usize, content: &str) {
        if let Some(msg) = self
            .messages
            .iter_mut()
            .skip(start)
            .find(|m| m.role == Role::User)
        {
            msg.content = content.to_string();
        } else {
            self.push_user_message(content);
        }
    }

    pub fn push_assistant_message(
        &mut self,
        content: &str,
        reasoning: &str,
        tool_calls: Option<Vec<lattice_core::types::ToolCall>>,
    ) {
        if let Some(ref calls) = tool_calls {
            for call in calls {
                self.tool_names
                    .insert(call.id.clone(), call.function.name.clone());
            }
        }
        self.messages.push(Message {
            role: Role::Assistant,
            content: content.to_string(),
            reasoning_content: if reasoning.is_empty() {
                None
            } else {
                Some(reasoning.to_string())
            },
            tool_calls,
            tool_call_id: None,
            name: None,
        });
    }

    pub fn push_tool_result(&mut self, call_id: &str, result: &str, max_size: Option<usize>) {
        let max = max_size.unwrap_or(1_048_576); // default 1MB
        let content = if result.len() > max {
            // Find safe UTF-8 boundary to avoid panicking in the middle of a
            // multi-byte character.
            let mut end = max;
            while end > 0 && !result.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}... (truncated to {} bytes)", &result[..end], max)
        } else {
            result.to_string()
        };
        self.messages.push(Message {
            role: Role::Tool,
            content,
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some(call_id.to_string()),
            name: self.tool_names.get(call_id).cloned(),
        });
    }

    /// Add tokens to the cumulative usage counter.
    /// Pop the last assistant message from the conversation state.
    /// Used when retrying after a mid-stream error — run_chat() already pushed
    /// an assistant message, and we need to undo that before retrying.
    pub fn pop_last_assistant_message(&mut self) {
        let is_assistant = self
            .messages
            .last()
            .map(|m| matches!(m.role, Role::Assistant))
            .unwrap_or(false);
        if is_assistant {
            self.messages.pop();
        }
    }

    pub fn add_token_usage(&mut self, tokens: u64) {
        self.token_usage += tokens;
    }

    /// Trim old non-system messages so the total estimated tokens
    /// are within the model's context window, keeping a safety margin.
    /// System messages (role=System) are always preserved.
    /// The most recent messages (user, assistant, tool) are kept.
    pub fn trim_messages(&mut self, context_length: u32, safety_margin_percent: u8) {
        let budget = (context_length as f64 * (1.0 - safety_margin_percent as f64 / 100.0)) as u32;

        let system_msgs: Vec<_> = self
            .messages
            .iter()
            .filter(|m| matches!(m.role, lattice_core::Role::System))
            .cloned()
            .collect();

        let mut non_system: Vec<_> = self
            .messages
            .iter()
            .filter(|m| !matches!(m.role, lattice_core::Role::System))
            .cloned()
            .collect();

        if non_system.len() <= 2 {
            return;
        }

        let estimate_msg = |msg: &lattice_core::types::Message| -> u32 {
            lattice_core::tokens::TokenEstimator::estimate_text_for_model(
                &msg.content,
                &self.resolved.api_model_id,
            )
        };

        // Strip leading orphaned tool messages (from prior incomplete trims)
        let sys_tokens: u32 = system_msgs.iter().map(estimate_msg).sum();
        let mut non_system_tokens: u32 = non_system.iter().map(estimate_msg).sum();
        while non_system.first().is_some_and(|m| m.role == lattice_core::Role::Tool) {
            let orphan = non_system.remove(0);
            non_system_tokens = non_system_tokens.saturating_sub(estimate_msg(&orphan));
        }

        // Remove oldest non-system messages until we fit. Keep a running token
        // total so large histories do not re-estimate the full window per loop.
        // When removing an assistant message that has tool_calls, also remove
        // the tool-result messages that follow it to avoid orphaned tool messages.
        while non_system.len() > 2 {
            if non_system_tokens + sys_tokens <= budget {
                break;
            }
            let removed = non_system.remove(0);
            non_system_tokens = non_system_tokens.saturating_sub(estimate_msg(&removed));

            // If the removed message had tool_calls, remove subsequent tool results
            if removed.role == lattice_core::Role::Assistant && removed.tool_calls.is_some() {
                while non_system.first().is_some_and(|m| m.role == lattice_core::Role::Tool) {
                    let tool_msg = non_system.remove(0);
                    non_system_tokens = non_system_tokens.saturating_sub(estimate_msg(&tool_msg));
                }
            }
        }

        // Rebuild: system first, then trimmed non-system
        self.messages = system_msgs;
        self.messages.extend(non_system);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_core::catalog::ApiProtocol;
    use std::collections::HashMap;

    fn make_resolved(context_length: u32) -> ResolvedModel {
        ResolvedModel {
            canonical_id: "test".into(),
            provider: "test".into(),
            api_key: None,
            base_url: "".into(),
            api_protocol: ApiProtocol::OpenAiChat,
            api_model_id: "test".into(),
            context_length,
            provider_specific: HashMap::new(),
            credential_status: lattice_core::CredentialStatus::Missing,
        }
    }

    #[test]
    fn test_trim_messages_removes_old_messages() {
        let mut state = AgentState::new(make_resolved(200)); // tiny context to force trimming

        state.push_system_message("You are a tester.");
        // Each message is ~200 chars = ~50 tokens by char/4, so a handful will
        // blow past the 200-token context with 15% margin (170 budget).
        let long_text = "x".repeat(200);
        for i in 0..20 {
            state.push_user_message(&format!("msg {}: {}", i, long_text));
            state.push_assistant_message(&format!("response {}: {}", i, long_text), "", None);
        }
        let before = state.messages.len();
        assert_eq!(before, 41); // 1 system + 40 user/assistant

        state.trim_messages(200, 15);
        let after = state.messages.len();
        assert!(
            after < before,
            "trim should reduce messages: {} -> {}",
            before,
            after
        );
        assert_eq!(
            state.messages[0].role,
            Role::System,
            "system message should be preserved first"
        );
        // At least 2 non-system messages should remain
        let non_system_count = state
            .messages
            .iter()
            .filter(|m| m.role != Role::System)
            .count();
        assert!(
            non_system_count >= 2,
            "should keep at least 2 non-system messages: got {}",
            non_system_count
        );
    }

    #[test]
    fn test_trim_messages_noop_if_within_budget() {
        let mut state = AgentState::new(make_resolved(131072));

        state.push_system_message("You are a helper.");
        state.push_user_message("Hello");
        state.push_assistant_message("Hi there!", "", None);

        let before = state.messages.clone();
        state.trim_messages(131072, 15);
        assert_eq!(
            state.messages, before,
            "messages should remain unchanged when within budget"
        );
    }

    #[test]
    fn test_trim_messages_always_keeps_minimum() {
        let mut state = AgentState::new(make_resolved(131072));

        state.push_system_message("System.");
        let long_text = "x".repeat(200);
        state.push_user_message(&format!("User 1: {}", long_text));
        state.push_assistant_message(&format!("Assistant 1: {}", long_text), "", None);
        state.push_user_message(&format!("User 2: {}", long_text));
        state.push_assistant_message(&format!("Assistant 2: {}", long_text), "", None);

        // tiny budget to force trimming, but 2 non-system minimum keeps 1 user + 1 assistant
        state.trim_messages(1, 0);
        assert!(state.messages.len() >= 3, "system + at least 2 non-system"); // 1 system + 2 non-system
    }

    #[test]
    fn test_replace_first_user_message_replaces_in_place() {
        let mut state = AgentState::new(make_resolved(131072));
        state.push_system_message("System.");
        state.push_user_message("original user message");
        state.push_assistant_message("response", "", None);

        state.replace_first_user_message("replaced user message");

        assert_eq!(state.messages.len(), 3, "message count should not grow");
        let user_msg = state
            .messages
            .iter()
            .find(|m| m.role == Role::User)
            .unwrap();
        assert_eq!(user_msg.content, "replaced user message");
    }

    #[test]
    fn test_replace_first_user_message_no_user_falls_back_to_push() {
        let mut state = AgentState::new(make_resolved(131072));
        state.push_system_message("System.");
        // No user message yet

        state.replace_first_user_message("first user");

        assert_eq!(
            state.messages.len(),
            2,
            "should have pushed a new user message"
        );
        let user_msg = state
            .messages
            .iter()
            .find(|m| m.role == Role::User)
            .unwrap();
        assert_eq!(user_msg.content, "first user");
    }

    #[test]
    fn test_upsert_user_message_from_preserves_older_user_messages() {
        let mut state = AgentState::new(make_resolved(131072));
        state.push_user_message("older user");
        state.push_assistant_message("older assistant", "", None);
        let active_start = state.messages.len();

        state.upsert_user_message_from(active_start, "compiled active turn");

        assert_eq!(state.messages[0].content, "older user");
        assert_eq!(state.messages[2].content, "compiled active turn");

        state.upsert_user_message_from(active_start, "recompiled active turn");

        assert_eq!(state.messages[0].content, "older user");
        assert_eq!(state.messages.len(), 3);
        assert_eq!(state.messages[2].content, "recompiled active turn");
    }

    #[test]
    fn test_push_system_message_inserts_before_seeded_history() {
        let mut state = AgentState::new(make_resolved(131072));
        state.push_user_message("older user");
        state.push_assistant_message("older assistant", "", None);

        state.push_system_message("system");

        assert_eq!(state.messages[0].role, Role::System);
        assert_eq!(state.messages[0].content, "system");
        assert_eq!(state.messages[1].content, "older user");
    }

    #[test]
    fn test_snapshot_restore_recovers_messages_and_usage() {
        let mut state = AgentState::new(make_resolved(131072));
        state.push_user_message("original");
        state.add_token_usage(42);
        let snapshot = state.snapshot();

        state.push_assistant_message("assistant", "", None);
        state.add_token_usage(9);
        state.restore(snapshot);

        assert_eq!(state.messages.len(), 1);
        assert_eq!(state.messages[0].content, "original");
        assert_eq!(state.token_usage, 42);
    }
}
