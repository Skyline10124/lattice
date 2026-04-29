use artemis_core::types::Message;
use artemis_core::types::Role;
use artemis_core::ResolvedModel;

pub struct AgentState {
    pub messages: Vec<Message>,
    pub resolved: ResolvedModel,
}

impl AgentState {
    pub fn new(resolved: ResolvedModel) -> Self {
        Self {
            messages: vec![],
            resolved,
        }
    }

    pub fn push_user_message(&mut self, content: &str) {
        self.messages.push(Message {
            role: Role::User,
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });
    }

    pub fn push_assistant_message(
        &mut self,
        content: &str,
        tool_calls: Option<Vec<artemis_core::types::ToolCall>>,
    ) {
        self.messages.push(Message {
            role: Role::Assistant,
            content: content.to_string(),
            tool_calls,
            tool_call_id: None,
            name: None,
        });
    }

    pub fn push_tool_result(&mut self, call_id: &str, result: &str, max_size: Option<usize>) {
        let max = max_size.unwrap_or(1_048_576); // default 1MB
        let content = if result.len() > max {
            format!("{}... (truncated to {} bytes)", &result[..max], max)
        } else {
            result.to_string()
        };
        self.messages.push(Message {
            role: Role::Tool,
            content,
            tool_calls: None,
            tool_call_id: Some(call_id.to_string()),
            name: None,
        });
    }
}
