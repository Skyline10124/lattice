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
}
