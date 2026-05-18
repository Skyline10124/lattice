//! These types define the direct wire contract for LATTICE LLM calls
//! and are independent of `lattice-core`.

use serde::{Deserialize, Serialize};

/// The role of a message participant in a conversation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// Details of a function call invoked by the model.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

impl FunctionCall {
    pub fn new(name: impl Into<String>, arguments: impl Into<String>) -> Self {
        FunctionCall {
            name: name.into(),
            arguments: arguments.into(),
        }
    }
}

/// A tool call made by the assistant, referencing a function to invoke.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub function: FunctionCall,
}

impl ToolCall {
    pub fn new(id: impl Into<String>, function: FunctionCall) -> Self {
        ToolCall {
            id: id.into(),
            function,
        }
    }
}

/// A message in a conversation between user and assistant.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Message {
    pub role: Role,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Message {
            role: Role::User,
            content: content.into(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Message {
            role: Role::Assistant,
            content: content.into(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Message {
            role: Role::System,
            content: content.into(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }
}

/// A tool definition providing a function specification to the model.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    /// JSON schema for the tool parameters.
    pub parameters: serde_json::Value,
}

impl ToolDefinition {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        ToolDefinition {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }
}

/// A chat completion request to be sent to an LLM.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChatRequest {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub target: ModelTarget,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u32>,
    /// OpenAI-specific: max completion tokens (replaces max_tokens for o-series)
    pub max_completion_tokens: Option<u32>,
    pub stream: bool,
}

impl ChatRequest {
    pub fn new(messages: impl Into<Vec<Message>>, target: ModelTarget) -> Self {
        ChatRequest {
            messages: messages.into(),
            tools: Vec::new(),
            target,
            temperature: None,
            max_tokens: None,
            max_completion_tokens: None,
            stream: false,
        }
    }
}

/// A chat completion response received from an LLM.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChatResponse {
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    pub finish_reason: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
}

/// A streaming LLM event for the API.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum LlmEvent {
    Started,
    Token {
        content: String,
    },
    Reasoning {
        content: String,
    },
    ToolCallStart {
        id: String,
        name: String,
    },
    ToolCallDelta {
        id: String,
        arguments_delta: String,
    },
    ToolCallEnd {
        id: String,
    },
    Done {
        finish_reason: String,
        usage: Option<TokenUsage>,
    },
    Error {
        message: String,
    },
    Citation {
        index: usize,
        cite_text: String,
    },
    Signature {
        signature: String,
    },
    GroundingChunk {
        text: String,
        url: Option<String>,
    },
    SafetyRating {
        category: String,
        probability: String,
    },
}

/// A model target specifying which model alias to route to.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelTarget {
    pub model: String,
    pub provider: Option<String>,
}

impl Default for ModelTarget {
    fn default() -> Self {
        ModelTarget {
            model: "sonnet".to_string(),
            provider: None,
        }
    }
}

impl ModelTarget {
    pub fn new(model: impl Into<String>) -> Self {
        ModelTarget {
            model: model.into(),
            provider: None,
        }
    }
}

/// Token usage statistics returned by the provider in the final stream chunk.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_serde_roundtrip() {
        let cases = vec![Role::System, Role::User, Role::Assistant, Role::Tool];
        for role in cases {
            let json = serde_json::to_string(&role).unwrap();
            let deserialized: Role = serde_json::from_str(&json).unwrap();
            assert_eq!(role, deserialized);
        }
    }

    #[test]
    fn role_serialization_variants() {
        assert_eq!(serde_json::to_string(&Role::System).unwrap(), "\"System\"");
        assert_eq!(serde_json::to_string(&Role::User).unwrap(), "\"User\"");
        assert_eq!(
            serde_json::to_string(&Role::Assistant).unwrap(),
            "\"Assistant\""
        );
        assert_eq!(serde_json::to_string(&Role::Tool).unwrap(), "\"Tool\"");
    }

    #[test]
    fn function_call_roundtrip() {
        let fc = FunctionCall::new("get_weather", r#"{"city": "Tokyo"}"#);
        let json = serde_json::to_string(&fc).unwrap();
        let deserialized: FunctionCall = serde_json::from_str(&json).unwrap();
        assert_eq!(fc, deserialized);
    }

    #[test]
    fn tool_call_roundtrip() {
        let tc = ToolCall::new(
            "call_abc123",
            FunctionCall::new("get_weather", r#"{"city":"Paris"}"#),
        );
        let json = serde_json::to_string(&tc).unwrap();
        let deserialized: ToolCall = serde_json::from_str(&json).unwrap();
        assert_eq!(tc, deserialized);
    }

    #[test]
    fn message_simple_roundtrip() {
        let msg = Message::user("Hello, world!");
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, deserialized);
    }

    #[test]
    fn message_with_tool_calls_roundtrip() {
        let msg = Message {
            role: Role::Assistant,
            content: String::new(),
            reasoning_content: None,
            tool_calls: Some(vec![ToolCall::new(
                "call_1",
                FunctionCall::new("search", r#"{"q": "rust"}"#),
            )]),
            tool_call_id: None,
            name: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, deserialized);
    }

    #[test]
    fn message_tool_result_roundtrip() {
        let msg = Message {
            role: Role::Tool,
            content: r#"{"result": "sunny"}"#.to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some("call_1".to_string()),
            name: Some("get_weather".to_string()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, deserialized);
    }

    #[test]
    fn tool_definition_roundtrip() {
        let td = ToolDefinition::new(
            "get_weather",
            "Get weather for a city",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "city": {"type": "string"}
                },
                "required": ["city"]
            }),
        );
        let json = serde_json::to_string(&td).unwrap();
        let deserialized: ToolDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(td, deserialized);
    }

    #[test]
    fn chat_request_roundtrip() {
        let req = ChatRequest {
            messages: vec![Message::user("hello")],
            tools: vec![],
            target: ModelTarget::default(),
            temperature: Some(0.7),
            max_tokens: None,
            max_completion_tokens: None,
            stream: false,
        };
        let json = serde_json::to_string(&req).unwrap();
        let deserialized: ChatRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req.messages, deserialized.messages);
        assert_eq!(req.target.model, deserialized.target.model);
    }

    #[test]
    fn chat_response_roundtrip() {
        let resp = ChatResponse {
            content: "Hello!".to_string(),
            reasoning_content: None,
            tool_calls: None,
            finish_reason: "stop".to_string(),
            model: "sonnet".to_string(),
            usage: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let deserialized: ChatResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp, deserialized);
    }

    #[test]
    fn llm_event_roundtrip() {
        let events = vec![
            LlmEvent::Started,
            LlmEvent::Token {
                content: "hello".to_string(),
            },
            LlmEvent::Done {
                finish_reason: "stop".to_string(),
                usage: None,
            },
        ];
        for event in events {
            let json = serde_json::to_string(&event).unwrap();
            let deserialized: LlmEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(event, deserialized);
        }
    }

    #[test]
    fn model_target_default() {
        let t = ModelTarget::default();
        assert_eq!(t.model, "sonnet");
        assert!(t.provider.is_none());
    }
}
