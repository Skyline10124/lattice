#![allow(deprecated)]
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use pyo3::prelude::*;

use crate::catalog::{ApiProtocol, CatalogProviderEntry, ModelCatalogEntry, ResolvedModel};
use crate::mock::MockProvider;
use crate::provider::{ChatRequest, ChatResponse, ModelEntry, ModelRegistry};
use crate::router::ModelRouter;
use crate::types::{FunctionCall, Message, Role, ToolCall, ToolDefinition};

#[pyclass(from_py_object)]
#[derive(Clone, Debug)]
pub struct PyResolvedModel {
    #[pyo3(get)]
    pub canonical_id: String,
    #[pyo3(get)]
    pub provider: String,
    #[pyo3(get)]
    pub api_key: Option<String>,
    #[pyo3(get)]
    pub base_url: String,
    #[pyo3(get)]
    pub api_protocol: String,
    #[pyo3(get)]
    pub api_model_id: String,
    #[pyo3(get)]
    pub context_length: u32,
}

#[pymethods]
impl PyResolvedModel {
    fn __repr__(&self) -> String {
        format!(
            "PyResolvedModel(canonical_id={}, provider={}, api_key={})",
            self.canonical_id,
            self.provider,
            self.api_key.as_ref().map(|_| "***").unwrap_or("None"),
        )
    }
}

impl From<&ResolvedModel> for PyResolvedModel {
    fn from(r: &ResolvedModel) -> Self {
        PyResolvedModel {
            canonical_id: r.canonical_id.clone(),
            provider: r.provider.clone(),
            api_key: r.api_key.clone(),
            base_url: r.base_url.clone(),
            api_protocol: format!("{:?}", r.api_protocol),
            api_model_id: r.api_model_id.clone(),
            context_length: r.context_length,
        }
    }
}

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
    default_model: Option<String>,
}

#[pyclass]
pub struct ArtemisEngine {
    runtime: Mutex<tokio::runtime::Runtime>,
    registry: Mutex<ModelRegistry>,
    state: Mutex<Option<EngineState>>,
    interrupted: Arc<AtomicBool>,
}

#[pymethods]
impl ArtemisEngine {
    #[new]
    fn new() -> Self {
        let router = ModelRouter::new();
        ArtemisEngine {
            runtime: Mutex::new(
                tokio::runtime::Runtime::new().expect("Failed to create tokio runtime")
            ),
            registry: Mutex::new(ModelRegistry::new(router)),
            state: Mutex::new(None),
            interrupted: Arc::new(AtomicBool::new(false)),
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
        let entry = ModelEntry {
            config: ModelCatalogEntry {
                canonical_id: name.to_string(),
                display_name: name.to_string(),
                description: String::new(),
                context_length: 131072,
                capabilities: vec![],
                providers: vec![CatalogProviderEntry {
                    provider_id: "mock".to_string(),
                    api_model_id: name.to_string(),
                    priority: 1,
                    weight: 1,
                    credential_keys: HashMap::new(),
                    base_url: Some("http://localhost".to_string()),
                    api_protocol: ApiProtocol::OpenAiChat,
                    provider_specific: HashMap::new(),
                }],
                aliases: vec![],
            },
            provider: Box::new(provider),
        };
        let mut registry = self.registry.lock().unwrap();
        registry.register(name, entry);
        Ok(())
    }

    fn set_model(&self, model_id: &str) -> PyResult<()> {
        let mut state = self.state.lock().unwrap();
        let s = state.get_or_insert_with(|| EngineState {
            tools: Vec::new(),
            last_response: None,
            default_model: None,
        });
        s.default_model = Some(model_id.to_string());
        Ok(())
    }

    fn get_model(&self) -> PyResult<Option<String>> {
        let state = self.state.lock().unwrap();
        Ok(state.as_ref().and_then(|s| s.default_model.clone()))
    }

    #[pyo3(signature = (messages, tools, model = None))]
    fn run_conversation(
        &self,
        py: Python<'_>,
        messages: Vec<Message>,
        tools: Vec<ToolDefinition>,
        model: Option<&str>,
    ) -> PyResult<Vec<Event>> {
        if let Some(model_id) = model {
            self.set_model(model_id)?;
        }
        self.run_once(py, messages, tools)
    }

    fn submit_tool_results(
        &self,
        py: Python<'_>,
        results: Vec<(String, String)>,
    ) -> PyResult<Vec<Event>> {
        let mut all_events = Vec::new();
        for (tool_call_id, result) in results {
            let events = self.submit_tool_result(py, tool_call_id, result)?;
            all_events.extend(events);
        }
        Ok(all_events)
    }

    fn interrupt(&self) {
        self.interrupted.store(true, Ordering::SeqCst);
    }

    fn run_once(
        &self,
        py: Python<'_>,
        messages: Vec<Message>,
        tools: Vec<ToolDefinition>,
    ) -> PyResult<Vec<Event>> {
        let model_id = {
            let state = self.state.lock().unwrap();
            match state.as_ref() {
                Some(s) => s.default_model.clone(),
                None => None,
            }
        };

        let (model_id, resolved) = {
            let registry = self.registry.lock().unwrap();
            if let Some(ref mid) = model_id {
                let entry = registry.get(mid).ok_or_else(|| {
                    pyo3::exceptions::PyRuntimeError::new_err(format!(
                        "Model '{}' not registered",
                        mid
                    ))
                })?;
                let resolved = ResolvedModel {
                    canonical_id: mid.clone(),
                    provider: "mock".to_string(),
                    api_key: None,
                    base_url: "http://localhost".to_string(),
                    api_protocol: ApiProtocol::OpenAiChat,
                    api_model_id: mid.clone(),
                    context_length: entry.config.context_length,
                    provider_specific: HashMap::new(),
                };
                Ok::<_, PyErr>((mid.clone(), resolved))
            } else {
                let ids = registry.list_models();
                if ids.is_empty() {
                    return Err(pyo3::exceptions::PyRuntimeError::new_err(
                        "No models registered",
                    ));
                }
                let mid = ids[0].clone();
                let resolved = ResolvedModel {
                    canonical_id: mid.clone(),
                    provider: "mock".to_string(),
                    api_key: None,
                    base_url: "http://localhost".to_string(),
                    api_protocol: ApiProtocol::OpenAiChat,
                    api_model_id: mid.clone(),
                    context_length: 131072,
                    provider_specific: HashMap::new(),
                };
                Ok((mid, resolved))
            }?
        };

        let request = ChatRequest::new(messages, tools.clone(), resolved);

        let response = self.block_on_model_chat(py, &model_id, request)?;

        let events = response_to_events(&response);

        {
            let mut state = self.state.lock().unwrap();
            *state = Some(EngineState {
                tools,
                last_response: Some(response),
                default_model: Some(model_id),
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
        let (model_id, tools, prev_messages) = {
            let state = self.state.lock().unwrap();
            match state.as_ref() {
                Some(s) => (
                    s.default_model.clone(),
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

        let model_id = model_id.ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err(
                "No default model set — call run_once() first",
            )
        })?;

        let resolved = {
            let registry = self.registry.lock().unwrap();
            let entry = registry.get(&model_id).ok_or_else(|| {
                pyo3::exceptions::PyRuntimeError::new_err(format!(
                    "Model '{}' not registered",
                    model_id
                ))
            })?;
            ResolvedModel {
                canonical_id: model_id.clone(),
                provider: "mock".to_string(),
                api_key: None,
                base_url: "http://localhost".to_string(),
                api_protocol: ApiProtocol::OpenAiChat,
                api_model_id: model_id.clone(),
                context_length: entry.config.context_length,
                provider_specific: HashMap::new(),
            }
        };

        let request = ChatRequest::new(prev_messages, tools, resolved);

        let response = self.block_on_model_chat(py, &model_id, request)?;

        let events = response_to_events(&response);

        {
            let mut state = self.state.lock().unwrap();
            if let Some(s) = state.as_mut() {
                s.last_response = Some(response);
            }
        }

        Ok(events)
    }

    fn register_model(
        &self,
        canonical_id: String,
        display_name: String,
        provider_id: String,
        api_model_id: String,
        base_url: String,
        api_protocol_str: String,
    ) -> PyResult<()> {
        let api_protocol = ApiProtocol::from_str(&api_protocol_str);
        let provider_entry = CatalogProviderEntry {
            provider_id: provider_id.clone(),
            api_model_id: api_model_id.clone(),
            priority: 1,
            weight: 1,
            credential_keys: HashMap::new(),
            base_url: Some(base_url.clone()),
            api_protocol: api_protocol.clone(),
            provider_specific: HashMap::new(),
        };
        let catalog_entry = ModelCatalogEntry {
            canonical_id: canonical_id.clone(),
            display_name,
            description: String::new(),
            context_length: 131072,
            capabilities: vec![],
            providers: vec![provider_entry],
            aliases: vec![],
        };

        let provider = MockProvider::new(&canonical_id);
        let model_entry = ModelEntry {
            config: catalog_entry.clone(),
            provider: Box::new(provider),
        };

        let mut registry = self.registry.lock().unwrap();
        registry.register_catalog_entry(catalog_entry);
        registry.register(&canonical_id, model_entry);
        Ok(())
    }

    #[pyo3(signature = (model_name, provider_override = None))]
    fn resolve_model(
        &self,
        model_name: &str,
        provider_override: Option<&str>,
    ) -> PyResult<PyResolvedModel> {
        let registry = self.registry.lock().unwrap();
        let resolved = registry
            .resolve(model_name, provider_override)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        Ok(PyResolvedModel::from(&resolved))
    }

    fn list_models(&self) -> PyResult<Vec<String>> {
        let registry = self.registry.lock().unwrap();
        Ok(registry.list_models())
    }

    fn list_authenticated_models(&self) -> PyResult<Vec<String>> {
        let registry = self.registry.lock().unwrap();
        Ok(registry.list_authenticated_models())
    }
}

impl ArtemisEngine {
    fn block_on_model_chat(
        &self,
        _py: Python<'_>,
        model_id: &str,
        request: ChatRequest,
    ) -> PyResult<ChatResponse> {
        let rt = self.runtime.lock().unwrap();
        let registry = self.registry.lock().unwrap();
        let entry = registry.get(model_id)
            .ok_or_else(|| pyo3::exceptions::PyRuntimeError::new_err(
                format!("Model '{}' not found", model_id),
            ))?;
        rt.block_on(entry.provider.chat(request))
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
