#![allow(deprecated)]
use std::sync::Mutex;

use pyo3::prelude::*;

use crate::mock::MockProvider;
use crate::provider::{ChatRequest, ChatResponse, ProviderRegistry};
use crate::types::{FunctionCall, Message, ProviderConfig, Role, ToolCall, ToolDefinition, TransportType};

#[pyclass(from_py_object)]
#[derive(Clone, Debug)]
pub struct ToolCallInfo {
    #[pyo3(get, set)]
    pub id: String,
    #[pyo3(get, set)]
    pub name: String,
    #[pyo3(get, set)]
    pub arguments: String,
}

#[pymethods]
impl ToolCallInfo {
    #[new]
    fn new(id: String, name: String, arguments: String) -> Self {
        ToolCallInfo { id, name, arguments }
    }

    fn __repr__(&self) -> String {
        format!("ToolCallInfo(id={}, name={}, arguments={})", self.id, self.name, self.arguments)
    }
}

impl From<&ToolCall> for ToolCallInfo {
    fn from(tc: &ToolCall) -> Self {
        ToolCallInfo {
            id: tc.id.clone(),
            name: tc.function.name.clone(),
            arguments: tc.function.arguments.clone(),
        }
    }
}

#[pyclass(from_py_object)]
#[derive(Clone, Debug)]
pub struct Event {
    #[pyo3(get, set)]
    pub kind: String,
    #[pyo3(get, set)]
    pub content: Option<String>,
    #[pyo3(get, set)]
    pub tool_calls: Option<Vec<ToolCallInfo>>,
    #[pyo3(get, set)]
    pub finish_reason: Option<String>,
}

#[pymethods]
impl Event {
    #[new]
    #[pyo3(signature = (kind, content=None, tool_calls=None, finish_reason=None))]
    fn new(
        kind: String,
        content: Option<String>,
        tool_calls: Option<Vec<ToolCallInfo>>,
        finish_reason: Option<String>,
    ) -> Self {
        Event { kind, content, tool_calls, finish_reason }
    }

    fn __repr__(&self) -> String {
        match &self.content {
            Some(c) => format!("Event(kind={}, content={})", self.kind, c),
            None => format!("Event(kind={}, tool_calls={})", self.kind, self.tool_calls.as_ref().map(|v| v.len()).unwrap_or(0)),
        }
    }
}

struct EngineState {
    tools: Vec<ToolDefinition>,
    last_response: Option<ChatResponse>,
    default_provider: String,
}

#[pyclass]
pub struct ArtemisEngine {
    runtime: Mutex<tokio::runtime::Runtime>,
    registry: Mutex<ProviderRegistry>,
    state: Mutex<Option<EngineState>>,
}

#[pymethods]
impl ArtemisEngine {
    #[new]
    fn new() -> Self {
        ArtemisEngine {
            runtime: Mutex::new(
                tokio::runtime::Runtime::new().expect("Failed to create tokio runtime")
            ),
            registry: Mutex::new(ProviderRegistry::new()),
            state: Mutex::new(None),
        }
    }

    fn add_mock_provider(&self, name: &str) -> PyResult<()> {
        let provider = MockProvider::new(name)
            .with_first_content("Hello from mock!")
            .with_first_tool_calls(vec![ToolCall {
                id: "call_mock_1".to_string(),
                function: FunctionCall {
                    name: "mock_tool".to_string(),
                    arguments: r#"{"query":"test"}"#.to_string(),
                },
            }])
            .with_final_content("Final response from mock!");
        let mut registry = self.registry.lock().unwrap();
        registry.register(name, Box::new(provider));
        Ok(())
    }

    fn run_once(
        &self,
        py: Python<'_>,
        messages: Vec<Message>,
        tools: Vec<ToolDefinition>,
    ) -> PyResult<Vec<Event>> {
        let provider_name = {
            let state = self.state.lock().unwrap();
            match state.as_ref() {
                Some(s) => s.default_provider.clone(),
                None => {
                    let registry = self.registry.lock().unwrap();
                    let names = registry.list();
                    if names.is_empty() {
                        return Err(pyo3::exceptions::PyRuntimeError::new_err(
                            "No providers registered",
                        ));
                    }
                    names[0].clone()
                }
            }
        };

        let request = ChatRequest {
            messages,
            tools: tools.clone(),
            model: "mock-model".to_string(),
            temperature: None,
            max_tokens: None,
            stream: false,
            provider_config: ProviderConfig {
                name: provider_name.clone(),
                api_base: "http://localhost".to_string(),
                api_key: None,
                transport: TransportType::ChatCompletions,
                extra_headers: None,
            },
        };

        let response = self.block_on_provider_chat(py, &provider_name, request)?;

        let events = response_to_events(&response);

        {
            let mut state = self.state.lock().unwrap();
            *state = Some(EngineState {
                tools,
                last_response: Some(response),
                default_provider: provider_name,
            });
        }

        Ok(events)
    }

    fn submit_tool_result(
        &self,
        py: Python<'_>,
        tool_call_id: String,
        result: String,
    ) -> PyResult<Vec<Event>> {
        let (provider_name, tools, prev_messages) = {
            let state = self.state.lock().unwrap();
            match state.as_ref() {
                Some(s) => (
                    s.default_provider.clone(),
                    s.tools.clone(),
                    match s.last_response.as_ref() {
                        Some(resp) => {
                            let mut msgs = Vec::new();
                            msgs.push(Message {
                                role: Role::Assistant,
                                content: resp.content.clone().unwrap_or_default(),
                                tool_calls: resp.tool_calls.clone(),
                                tool_call_id: None,
                                name: None,
                            });
                            msgs.push(Message {
                                role: Role::Tool,
                                content: result,
                                tool_calls: None,
                                tool_call_id: Some(tool_call_id),
                                name: None,
                            });
                            msgs
                        }
                        None => vec![],
                    },
                ),
                None => {
                    return Err(pyo3::exceptions::PyRuntimeError::new_err(
                        "No active conversation — call run_once() first",
                    ));
                }
            }
        };

        let request = ChatRequest {
            messages: prev_messages,
            tools,
            model: "mock-model".to_string(),
            temperature: None,
            max_tokens: None,
            stream: false,
            provider_config: ProviderConfig {
                name: provider_name.clone(),
                api_base: "http://localhost".to_string(),
                api_key: None,
                transport: TransportType::ChatCompletions,
                extra_headers: None,
            },
        };

        let response = self.block_on_provider_chat(py, &provider_name, request)?;

        let events = response_to_events(&response);

        {
            let mut state = self.state.lock().unwrap();
            if let Some(s) = state.as_mut() {
                s.last_response = Some(response);
            }
        }

        Ok(events)
    }

    fn list_providers(&self) -> Vec<String> {
        self.registry.lock().unwrap().list()
    }
}

impl ArtemisEngine {
    fn block_on_provider_chat(
        &self,
        _py: Python<'_>,
        provider_name: &str,
        request: ChatRequest,
    ) -> PyResult<ChatResponse> {
        let rt = self.runtime.lock().unwrap();
        let registry = self.registry.lock().unwrap();
        let provider = registry.get(provider_name)
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err(
                format!("Provider '{}' not found", provider_name),
            ))?;
        rt.block_on(provider.chat(request))
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))
    }
}

fn response_to_events(response: &ChatResponse) -> Vec<Event> {
    let mut events = Vec::new();

    if let Some(ref content) = response.content {
        if !content.is_empty() {
            events.push(Event {
                kind: "token".to_string(),
                content: Some(content.clone()),
                tool_calls: None,
                finish_reason: None,
            });
        }
    }

    if let Some(ref tool_calls) = response.tool_calls {
        let infos: Vec<ToolCallInfo> = tool_calls.iter().map(ToolCallInfo::from).collect();
        events.push(Event {
            kind: "tool_call_required".to_string(),
            content: None,
            tool_calls: Some(infos),
            finish_reason: None,
        });
    }

    events.push(Event {
        kind: "done".to_string(),
        content: None,
        tool_calls: None,
        finish_reason: Some(response.finish_reason.clone()),
    });

    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_call_info_from_tool_call() {
        let tc = ToolCall {
            id: "call_1".to_string(),
            function: FunctionCall {
                name: "get_weather".to_string(),
                arguments: r#"{"city":"Paris"}"#.to_string(),
            },
        };
        let info = ToolCallInfo::from(&tc);
        assert_eq!(info.id, "call_1");
        assert_eq!(info.name, "get_weather");
        assert_eq!(info.arguments, r#"{"city":"Paris"}"#);
    }

    #[test]
    fn test_event_new_token() {
        let event = Event::new("token".to_string(), Some("hello".to_string()), None, None);
        assert_eq!(event.kind, "token");
        assert_eq!(event.content, Some("hello".to_string()));
    }

    #[test]
    fn test_event_new_done() {
        let event = Event::new("done".to_string(), None, None, Some("stop".to_string()));
        assert_eq!(event.kind, "done");
        assert_eq!(event.finish_reason, Some("stop".to_string()));
    }

    #[test]
    fn test_response_to_events_content_only() {
        let response = ChatResponse {
            content: Some("Hello!".to_string()),
            tool_calls: None,
            usage: None,
            finish_reason: "stop".to_string(),
            model: "mock".to_string(),
        };
        let events = response_to_events(&response);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "token");
        assert_eq!(events[0].content, Some("Hello!".to_string()));
        assert_eq!(events[1].kind, "done");
        assert_eq!(events[1].finish_reason, Some("stop".to_string()));
    }

    #[test]
    fn test_response_to_events_with_tool_calls() {
        let response = ChatResponse {
            content: Some("Let me check.".to_string()),
            tool_calls: Some(vec![ToolCall {
                id: "call_1".to_string(),
                function: FunctionCall {
                    name: "search".to_string(),
                    arguments: "{}".to_string(),
                },
            }]),
            usage: None,
            finish_reason: "tool_calls".to_string(),
            model: "mock".to_string(),
        };
        let events = response_to_events(&response);
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].kind, "token");
        assert_eq!(events[1].kind, "tool_call_required");
        assert!(events[1].tool_calls.is_some());
        assert_eq!(events[1].tool_calls.as_ref().unwrap().len(), 1);
        assert_eq!(events[2].kind, "done");
    }

    #[test]
    fn test_response_to_events_empty_content() {
        let response = ChatResponse {
            content: Some(String::new()),
            tool_calls: None,
            usage: None,
            finish_reason: "stop".to_string(),
            model: "mock".to_string(),
        };
        let events = response_to_events(&response);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "done");
    }

    #[test]
    fn test_response_to_events_none_content() {
        let response = ChatResponse {
            content: None,
            tool_calls: None,
            usage: None,
            finish_reason: "stop".to_string(),
            model: "mock".to_string(),
        };
        let events = response_to_events(&response);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "done");
    }
}
