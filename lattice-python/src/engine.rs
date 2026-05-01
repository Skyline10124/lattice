use std::collections::HashMap;
use std::sync::LazyLock;
use std::sync::Mutex;

use lattice_core::catalog::{CredentialStatus, ResolvedModel};
use lattice_core::provider::ChatResponse;
use lattice_core::router::ModelRouter;
use lattice_core::types::{Message, Role, ToolDefinition};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList};
use pyo3::Py;

use crate::errors::convert_core_error;

// ---------------------------------------------------------------------------
// Shared tokio runtime for bridging sync → async in PyO3 methods
// ---------------------------------------------------------------------------

static SHARED_RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Runtime::new()
        .expect("Failed to create shared tokio runtime for Python bindings")
});

/// Run an async future synchronously, safely handling nested runtime contexts.
fn run_async<F, T>(f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    if let Ok(_handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(|| SHARED_RUNTIME.block_on(f))
    } else {
        SHARED_RUNTIME.block_on(f)
    }
}

// ---------------------------------------------------------------------------
// Conversion helpers: Python → Rust → Python
// ---------------------------------------------------------------------------

/// Convert a list of Python message dicts into Rust `Message` values.
fn messages_from_py(
    py: Python<'_>,
    messages: Vec<HashMap<String, Py<PyAny>>>,
) -> PyResult<Vec<Message>> {
    let mut result = Vec::with_capacity(messages.len());
    for m in messages {
        let role_str: String = m
            .get("role")
            .ok_or_else(|| PyValueError::new_err("Each message must have a 'role' field"))?
            .extract(py)?;

        let role = match role_str.to_lowercase().as_str() {
            "system" => Role::System,
            "user" => Role::User,
            "assistant" => Role::Assistant,
            "tool" => Role::Tool,
            other => {
                return Err(PyValueError::new_err(format!(
                    "Invalid role '{}'. Must be one of: system, user, assistant, tool",
                    other
                )));
            }
        };

        let content: String = m
            .get("content")
            .map(|v| v.extract::<String>(py).unwrap_or_default())
            .unwrap_or_default();

        result.push(Message::new(role, content, None, None, None));
    }
    Ok(result)
}

/// Convert a list of Python tool-definition dicts into Rust `ToolDefinition` values.
fn tools_from_py(
    py: Python<'_>,
    tools: Vec<HashMap<String, Py<PyAny>>>,
) -> PyResult<Vec<ToolDefinition>> {
    let json_mod = py.import("json")?;
    let mut result = Vec::with_capacity(tools.len());

    for t in tools {
        let name: String = t
            .get("name")
            .ok_or_else(|| PyValueError::new_err("Each tool must have a 'name' field"))?
            .extract(py)?;

        let description: String = t
            .get("description")
            .map(|v| v.extract::<String>(py).unwrap_or_default())
            .unwrap_or_default();

        let parameters = match t.get("parameters") {
            Some(params) => {
                let json_str: String = json_mod.call_method1("dumps", (params,))?.extract()?;
                serde_json::from_str(&json_str).map_err(|e| {
                    PyValueError::new_err(format!(
                        "Invalid JSON schema in tool '{}' parameters: {}",
                        name, e
                    ))
                })?
            }
            None => serde_json::Value::Null,
        };

        result.push(ToolDefinition::new(name, description, parameters));
    }
    Ok(result)
}

/// Convert a Rust `ChatResponse` into a Python dict.
fn chat_response_to_py(py: Python<'_>, response: ChatResponse) -> PyResult<Py<PyAny>> {
    let dict = PyDict::new(py);

    if let Some(ref content) = response.content {
        dict.set_item("content", content)?;
    }
    if let Some(ref reasoning) = response.reasoning_content {
        dict.set_item("reasoning_content", reasoning)?;
    }
    dict.set_item("finish_reason", &response.finish_reason)?;
    dict.set_item("model", &response.model)?;

    if let Some(ref usage) = response.usage {
        let usage_dict = PyDict::new(py);
        usage_dict.set_item("prompt_tokens", usage.prompt_tokens)?;
        usage_dict.set_item("completion_tokens", usage.completion_tokens)?;
        usage_dict.set_item("total_tokens", usage.total_tokens)?;
        dict.set_item("usage", usage_dict)?;
    }

    if let Some(ref tool_calls) = response.tool_calls {
        let tc_list = PyList::empty(py);
        for tc in tool_calls {
            let tc_dict = PyDict::new(py);
            tc_dict.set_item("id", &tc.id)?;
            let fn_dict = PyDict::new(py);
            fn_dict.set_item("name", &tc.function.name)?;
            fn_dict.set_item("arguments", &tc.function.arguments)?;
            tc_dict.set_item("function", fn_dict)?;
            tc_list.append(tc_dict)?;
        }
        dict.set_item("tool_calls", tc_list)?;
    }

    Ok(dict.into())
}

/// Python-facing model resolver.
#[pyclass]
pub struct LatticeEngine {
    router: Mutex<ModelRouter>,
}

#[pymethods]
impl LatticeEngine {
    #[new]
    pub fn new() -> Self {
        Self {
            router: Mutex::new(ModelRouter::new()),
        }
    }

    /// Resolve a model name to connection details.
    /// Rejects non-localhost HTTP base URLs for security.
    pub fn resolve_model(&self, model: &str) -> PyResult<PyResolvedModel> {
        let resolved = self
            .router
            .lock()
            .unwrap()
            .resolve(model, None)
            .map_err(convert_core_error)?;

        // Security: reject non-localhost HTTP
        lattice_core::router::validate_base_url(&resolved.base_url).map_err(convert_core_error)?;

        Ok(PyResolvedModel { inner: resolved })
    }

    /// List all canonical model IDs.
    pub fn list_models(&self) -> Vec<String> {
        self.router.lock().unwrap().list_models()
    }

    /// List models with valid credentials.
    pub fn list_authenticated_models(&self) -> Vec<String> {
        self.router.lock().unwrap().list_authenticated_models()
    }

    /// Send messages to a resolved model and return the complete response.
    ///
    /// Args:
    ///     resolved: A PyResolvedModel from `resolve_model()`.
    ///     messages: List of dicts with `{"role": ..., "content": ...}`.
    ///               Roles: "system", "user", "assistant", "tool".
    ///     tools: Optional list of tool definition dicts, each with
    ///            `{"name": ..., "description": ..., "parameters": {...}}`.
    ///
    /// Returns: A dict with keys `content`, `finish_reason`, `model`,
    ///          and optionally `usage`, `tool_calls`, `reasoning_content`.
    #[pyo3(signature = (resolved, messages, tools=None))]
    fn chat_complete(
        &self,
        py: Python<'_>,
        resolved: &PyResolvedModel,
        messages: Vec<HashMap<String, Py<PyAny>>>,
        tools: Option<Vec<HashMap<String, Py<PyAny>>>>,
    ) -> PyResult<Py<PyAny>> {
        let msgs = messages_from_py(py, messages)?;
        let tool_defs = match tools {
            Some(t) => tools_from_py(py, t)?,
            None => Vec::new(),
        };

        let response = run_async(lattice_core::chat_complete(
            &resolved.inner,
            &msgs,
            &tool_defs,
        ))
        .map_err(convert_core_error)?;

        chat_response_to_py(py, response)
    }
}

/// Python-facing resolved model (read-only).
#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct PyResolvedModel {
    inner: ResolvedModel,
}

#[pymethods]
impl PyResolvedModel {
    #[getter]
    pub fn canonical_id(&self) -> &str {
        &self.inner.canonical_id
    }

    #[getter]
    pub fn provider(&self) -> &str {
        &self.inner.provider
    }

    #[getter]
    pub fn api_model_id(&self) -> &str {
        &self.inner.api_model_id
    }

    #[getter]
    pub fn context_length(&self) -> u32 {
        self.inner.context_length
    }

    #[getter]
    pub fn credential_status(&self) -> String {
        match self.inner.credential_status {
            CredentialStatus::Present => "present".to_string(),
            CredentialStatus::NotRequired => "not_required".to_string(),
            CredentialStatus::Missing => "missing".to_string(),
        }
    }

    fn __repr__(&self) -> String {
        let key_masked = self.inner.api_key.as_ref().map(|_| "***");
        format!(
            "PyResolvedModel(canonical_id='{}', provider='{}', api_key={:?}, credential_status='{}')",
            self.inner.canonical_id,
            self.inner.provider,
            key_masked,
            match self.inner.credential_status {
                CredentialStatus::Present => "present",
                CredentialStatus::NotRequired => "not_required",
                CredentialStatus::Missing => "missing",
            },
        )
    }
}
