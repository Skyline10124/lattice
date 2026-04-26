use pyo3::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// The role of a message participant in a conversation.
#[pyclass(from_py_object)]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[pymethods]
impl Role {
    fn __repr__(&self) -> String {
        match self {
            Role::System => "Role.System",
            Role::User => "Role.User",
            Role::Assistant => "Role.Assistant",
            Role::Tool => "Role.Tool",
        }
        .to_string()
    }

    fn __eq__(&self, other: &Self) -> bool {
        self == other
    }
}

/// Details of a function call invoked by the model.
#[pyclass(from_py_object)]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FunctionCall {
    #[pyo3(get, set)]
    pub name: String,
    #[pyo3(get, set)]
    pub arguments: String,
}

#[pymethods]
impl FunctionCall {
    #[new]
    fn new(name: String, arguments: String) -> Self {
        FunctionCall { name, arguments }
    }

    fn __repr__(&self) -> String {
        format!(
            "FunctionCall(name={}, arguments={})",
            self.name, self.arguments
        )
    }
}

/// A tool call made by the assistant, referencing a function to invoke.
#[pyclass(from_py_object)]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    #[pyo3(get, set)]
    pub id: String,
    #[pyo3(get, set)]
    pub function: FunctionCall,
}

#[pymethods]
impl ToolCall {
    #[new]
    fn new(id: String, function: FunctionCall) -> Self {
        ToolCall { id, function }
    }

    fn __repr__(&self) -> String {
        format!("ToolCall(id={}, function={})", self.id, self.function.name)
    }
}

/// A message in a conversation between user and assistant.
#[pyclass(from_py_object)]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Message {
    #[pyo3(get, set)]
    pub role: Role,
    #[pyo3(get, set)]
    pub content: String,
    #[pyo3(get, set)]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[pyo3(get, set)]
    pub tool_call_id: Option<String>,
    #[pyo3(get, set)]
    pub name: Option<String>,
}

#[pymethods]
impl Message {
    #[new]
    #[pyo3(signature = (role, content, tool_calls=None, tool_call_id=None, name=None))]
    fn new(
        role: Role,
        content: String,
        tool_calls: Option<Vec<ToolCall>>,
        tool_call_id: Option<String>,
        name: Option<String>,
    ) -> Self {
        Message {
            role,
            content,
            tool_calls,
            tool_call_id,
            name,
        }
    }

    fn __repr__(&self) -> String {
        let content_preview = if self.content.len() > 60 {
            format!("{}...", &self.content[..60])
        } else {
            self.content.clone()
        };
        format!(
            "Message(role={:?}, content={}, tool_calls={})",
            self.role,
            content_preview,
            self.tool_calls.as_ref().map(|v| v.len()).unwrap_or(0),
        )
    }
}

/// A tool definition providing a function specification to the model.
#[pyclass(from_py_object)]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolDefinition {
    #[pyo3(get, set)]
    pub name: String,
    #[pyo3(get, set)]
    pub description: String,
    /// JSON schema for the tool parameters. Accessed from Python via get_parameters/set_parameters.
    pub parameters: serde_json::Value,
}

#[pymethods]
impl ToolDefinition {
    #[new]
    #[pyo3(signature = (name, description, parameters = "{}"))]
    fn new(name: String, description: String, parameters: &str) -> Self {
        let params: serde_json::Value =
            serde_json::from_str(parameters).unwrap_or(serde_json::Value::Object(Default::default()));
        ToolDefinition {
            name,
            description,
            parameters: params,
        }
    }

    fn get_parameters(&self) -> String {
        serde_json::to_string(&self.parameters).unwrap_or_default()
    }

    fn set_parameters(&mut self, params: String) {
        if let Ok(val) = serde_json::from_str(&params) {
            self.parameters = val;
        }
    }

    fn __repr__(&self) -> String {
        let desc_preview = if self.description.len() > 60 {
            format!("{}...", &self.description[..60])
        } else {
            self.description.clone()
        };
        format!(
            "ToolDefinition(name={}, description={})",
            self.name, desc_preview
        )
    }
}

/// The type of API transport to use for a provider.
#[pyclass(from_py_object)]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum TransportType {
    #[serde(rename = "chat_completions")]
    ChatCompletions,
    #[serde(rename = "anthropic")]
    Anthropic,
    #[serde(rename = "gemini")]
    Gemini,
    #[serde(rename = "bedrock")]
    Bedrock,
    #[serde(rename = "codex")]
    Codex,
}

#[pymethods]
impl TransportType {
    fn __repr__(&self) -> String {
        match self {
            TransportType::ChatCompletions => "TransportType.ChatCompletions",
            TransportType::Anthropic => "TransportType.Anthropic",
            TransportType::Gemini => "TransportType.Gemini",
            TransportType::Bedrock => "TransportType.Bedrock",
            TransportType::Codex => "TransportType.Codex",
        }
        .to_string()
    }

    fn __eq__(&self, other: &Self) -> bool {
        self == other
    }
}

/// Configuration for connecting to a model provider.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ProviderConfig {
    pub name: String,
    pub api_base: String,
    pub api_key: Option<String>,
    pub transport: TransportType,
    pub extra_headers: Option<HashMap<String, String>>,
}

/// Configuration for a specific model.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ModelConfig {
    pub model_id: String,
    pub provider: String,
    pub context_window: u32,
    pub max_tokens: u32,
    pub supports_streaming: bool,
    pub supports_tools: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_role_roundtrip() {
        let cases = vec![
            Role::System,
            Role::User,
            Role::Assistant,
            Role::Tool,
        ];
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
    fn test_transport_type_roundtrip() {
        let cases = vec![
            TransportType::ChatCompletions,
            TransportType::Anthropic,
            TransportType::Gemini,
            TransportType::Bedrock,
            TransportType::Codex,
        ];
        for tt in cases {
            let json = serde_json::to_string(&tt).unwrap();
            let deserialized: TransportType = serde_json::from_str(&json).unwrap();
            assert_eq!(tt, deserialized);
        }
    }

    #[test]
    fn test_provider_config_roundtrip() {
        let config = ProviderConfig {
            name: "openai".into(),
            api_base: "https://api.openai.com/v1".into(),
            api_key: Some("sk-...".into()),
            transport: TransportType::ChatCompletions,
            extra_headers: Some(HashMap::from([("X-Custom".into(), "value".into())])),
        };
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: ProviderConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, deserialized);
    }

    #[test]
    fn test_model_config_roundtrip() {
        let config = ModelConfig {
            model_id: "gpt-4o".into(),
            provider: "openai".into(),
            context_window: 128000,
            max_tokens: 4096,
            supports_streaming: true,
            supports_tools: true,
        };
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: ModelConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, deserialized);
    }

    #[test]
    fn test_role_serialization_variants() {
        assert_eq!(
            serde_json::to_string(&Role::System).unwrap(),
            "\"System\""
        );
        assert_eq!(
            serde_json::to_string(&Role::User).unwrap(),
            "\"User\""
        );
        assert_eq!(
            serde_json::to_string(&Role::Assistant).unwrap(),
            "\"Assistant\""
        );
        assert_eq!(
            serde_json::to_string(&Role::Tool).unwrap(),
            "\"Tool\""
        );
    }
}
