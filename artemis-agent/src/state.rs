use std::collections::HashMap;

use artemis_core::types::Message;
use artemis_core::types::Role;
use artemis_core::ResolvedModel;

pub struct AgentState {
    pub messages: Vec<Message>,
    pub resolved: ResolvedModel,
    /// Cumulative total tokens used across all turns.
    pub token_usage: u64,
    /// Maps tool_call_id to function_name so push_tool_result can set the
    /// correct `name` field (required by Gemini for functionResponse.name).
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
        self.messages.push(Message {
            role: Role::System,
            content: content.to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });
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

    pub fn push_assistant_message(
        &mut self,
        content: &str,
        reasoning: &str,
        tool_calls: Option<Vec<artemis_core::types::ToolCall>>,
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
            .filter(|m| matches!(m.role, artemis_core::Role::System))
            .cloned()
            .collect();

        let mut non_system: Vec<_> = self
            .messages
            .iter()
            .filter(|m| !matches!(m.role, artemis_core::Role::System))
            .cloned()
            .collect();

        if non_system.len() <= 2 {
            return;
        }

        let estimate = |msgs: &[artemis_core::types::Message]| -> u32 {
            msgs.iter()
                .map(|m| {
                    artemis_core::tokens::TokenEstimator::estimate_text_for_model(
                        &m.content,
                        &self.resolved.api_model_id,
                    )
                })
                .sum()
        };

        let sys_tokens = estimate(&system_msgs);

        // Remove oldest non-system messages until we fit
        while non_system.len() > 2 {
            let current = estimate(&non_system);
            if current + sys_tokens <= budget {
                break;
            }
            non_system.remove(0);
        }

        // Rebuild: system first, then trimmed non-system
        self.messages = system_msgs;
        self.messages.extend(non_system);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use artemis_core::catalog::ApiProtocol;
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
        let mut state = AgentState::new(make_resolved(100)); // tiny context
        let long_text = "x".repeat(200); // ~50 tokens each

        state.push_system_message("System.");
        state.push_user_message(&format!("User 1: {}", long_text));
        state.push_assistant_message(&format!("Assistant 1: {}", long_text), "", None);
        state.push_user_message(&format!("User 2: {}", long_text));
        state.push_assistant_message(&format!("Assistant 2: {}", long_text), "", None);

        state.trim_messages(100, 10);

        let non_system_count = state
            .messages
            .iter()
            .filter(|m| m.role != Role::System)
            .count();
        assert_eq!(
            non_system_count, 2,
            "should keep at least 2 non-system messages: got {}",
            non_system_count
        );
        assert_eq!(state.messages[0].role, Role::System);
    }
}
