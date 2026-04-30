use serde::{Deserialize, Serialize};

pub use crate::catalog::ApiProtocol;

/// The role of a message participant in a conversation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// Details of a function call invoked by the model.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

impl FunctionCall {
    pub fn new(name: String, arguments: String) -> Self {
        FunctionCall { name, arguments }
    }
}

/// A tool call made by the assistant, referencing a function to invoke.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub function: FunctionCall,
}

impl ToolCall {
    pub fn new(id: String, function: FunctionCall) -> Self {
        ToolCall { id, function }
    }
}

/// A message in a conversation between user and assistant.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub role: Role,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tool_call_id: Option<String>,
    pub name: Option<String>,
}

impl Message {
    pub fn new(
        role: Role,
        content: String,
        tool_calls: Option<Vec<ToolCall>>,
        tool_call_id: Option<String>,
        name: Option<String>,
    ) -> Self {
        Message {
            role,
            content,
            reasoning_content: None,
            tool_calls,
            tool_call_id,
            name,
        }
    }

    /// Attach reasoning/thinking content to this message.
    pub fn with_reasoning(mut self, reasoning: String) -> Self {
        self.reasoning_content = Some(reasoning);
        self
    }
}

/// A tool definition providing a function specification to the model.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    /// JSON schema for the tool parameters.
    pub parameters: serde_json::Value,
}

impl ToolDefinition {
    /// Create a new ToolDefinition with JSON parameter validation.
    pub fn new(name: String, description: String, parameters: serde_json::Value) -> Self {
        ToolDefinition {
            name,
            description,
            parameters,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_role_roundtrip() {
        let cases = vec![Role::System, Role::User, Role::Assistant, Role::Tool];
        for role in cases {
            let json = serde_json::to_string(&role).unwrap();
            let deserialized: Role = serde_json::from_str(&json).unwrap();
            assert_eq!(role, deserialized);
        }
    }

    #[test]
    fn test_function_call_roundtrip() {
        let fc = FunctionCall {
            name: "get_weather".into(),
            arguments: r#"{"city": "Tokyo"}"#.into(),
        };
        let json = serde_json::to_string(&fc).unwrap();
        let deserialized: FunctionCall = serde_json::from_str(&json).unwrap();
        assert_eq!(fc, deserialized);
    }

    #[test]
    fn test_tool_call_roundtrip() {
        let tc = ToolCall {
            id: "call_abc123".into(),
            function: FunctionCall {
                name: "get_weather".into(),
                arguments: r#"{"city": "Paris"}"#.into(),
            },
        };
        let json = serde_json::to_string(&tc).unwrap();
        let deserialized: ToolCall = serde_json::from_str(&json).unwrap();
        assert_eq!(tc, deserialized);
    }

    #[test]
    fn test_message_simple_roundtrip() {
        let msg = Message {
            role: Role::User,
            content: "Hello, world!".into(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, deserialized);
    }

    #[test]
    fn test_message_with_tool_calls_roundtrip() {
        let msg = Message {
            role: Role::Assistant,
            content: String::new(),
            reasoning_content: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_1".into(),
                function: FunctionCall {
                    name: "search".into(),
                    arguments: r#"{"q": "rust"}"#.into(),
                },
            }]),
            tool_call_id: None,
            name: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, deserialized);
    }

    #[test]
    fn test_message_tool_result_roundtrip() {
        let msg = Message {
            role: Role::Tool,
            content: r#"{"result": "sunny"}"#.into(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some("call_1".into()),
            name: Some("get_weather".into()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, deserialized);
    }

    #[test]
    fn test_tool_definition_roundtrip() {
        let td = ToolDefinition {
            name: "get_weather".into(),
            description: "Get weather for a city".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "city": {"type": "string"}
                },
                "required": ["city"]
            }),
        };
        let json = serde_json::to_string(&td).unwrap();
        let deserialized: ToolDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(td, deserialized);
    }

    #[test]
    fn test_role_serialization_variants() {
        assert_eq!(serde_json::to_string(&Role::System).unwrap(), "\"System\"");
        assert_eq!(serde_json::to_string(&Role::User).unwrap(), "\"User\"");
        assert_eq!(
            serde_json::to_string(&Role::Assistant).unwrap(),
            "\"Assistant\""
        );
        assert_eq!(serde_json::to_string(&Role::Tool).unwrap(), "\"Tool\"");
    }
}
