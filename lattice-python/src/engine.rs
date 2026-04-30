use lattice_core::catalog::ResolvedModel;
use lattice_core::router::ModelRouter;
use pyo3::prelude::*;
use std::sync::Mutex;

use crate::errors::convert_core_error;

/// Python-facing model resolver.
#[pyclass]
pub struct ArtemisEngine {
    router: Mutex<ModelRouter>,
}

#[pymethods]
impl ArtemisEngine {
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

    fn __repr__(&self) -> String {
        let key_masked = self.inner.api_key.as_ref().map(|_| "***");
        format!(
            "PyResolvedModel(canonical_id='{}', provider='{}', api_key={:?})",
            self.inner.canonical_id, self.inner.provider, key_masked
        )
    }
}
