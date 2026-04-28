use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use artemis_core::errors::ArtemisError as CoreError;

create_exception!(artemis_core, ArtemisError, PyException);
create_exception!(artemis_core, RateLimitError, ArtemisError);
create_exception!(artemis_core, AuthenticationError, ArtemisError);
create_exception!(artemis_core, ModelNotFoundError, ArtemisError);
create_exception!(artemis_core, ProviderUnavailableError, ArtemisError);
create_exception!(artemis_core, ContextWindowExceededError, ArtemisError);
create_exception!(artemis_core, ToolExecutionError, ArtemisError);
create_exception!(artemis_core, StreamingError, ArtemisError);
create_exception!(artemis_core, ConfigError, ArtemisError);
create_exception!(artemis_core, NetworkError, ArtemisError);

/// Convert an artemis-core `ArtemisError` into a Python `PyErr`.
///
/// Uses `Python::try_attach` because this crate is always loaded as a
/// Python extension module, so the GIL is held whenever this is called.
pub fn convert_core_error(err: CoreError) -> PyErr {
    Python::try_attach(|py| match err {
        CoreError::RateLimit { retry_after, provider } => {
            let msg = format!("Rate limit exceeded for provider '{}'", provider);
            let e = PyErr::new::<RateLimitError, _>(msg);
            let v = e.value(py);
            let _ = v.setattr("retry_after", retry_after);
            let _ = v.setattr("provider", provider);
            e
        }
        CoreError::Authentication { provider } => {
            let msg = format!("Authentication failed for provider '{}'", provider);
            let e = PyErr::new::<AuthenticationError, _>(msg);
            let _ = e.value(py).setattr("provider", provider);
            e
        }
        CoreError::ModelNotFound { model } => {
            let msg = format!("Model '{}' not found", model);
            let e = PyErr::new::<ModelNotFoundError, _>(msg);
            let _ = e.value(py).setattr("model", model);
            e
        }
        CoreError::ProviderUnavailable { provider, reason } => {
            let msg = format!("Provider '{}' unavailable: {}", provider, reason);
            let e = PyErr::new::<ProviderUnavailableError, _>(msg);
            let v = e.value(py);
            let _ = v.setattr("provider", provider);
            let _ = v.setattr("reason", reason);
            e
        }
        CoreError::ContextWindowExceeded { tokens, limit } => {
            let msg = format!("Context window exceeded: {} tokens (limit {})", tokens, limit);
            let e = PyErr::new::<ContextWindowExceededError, _>(msg);
            let v = e.value(py);
            let _ = v.setattr("tokens", tokens);
            let _ = v.setattr("limit", limit);
            e
        }
        CoreError::ToolExecution { tool, message } => {
            let msg = format!("Tool '{}' execution failed: {}", tool, message);
            let e = PyErr::new::<ToolExecutionError, _>(msg);
            let v = e.value(py);
            let _ = v.setattr("tool", tool);
            let _ = v.setattr("message", message);
            e
        }
        CoreError::Streaming { message } => {
            let e = PyErr::new::<StreamingError, _>(message.clone());
            let _ = e.value(py).setattr("message", message);
            e
        }
        CoreError::Config { message } => {
            let e = PyErr::new::<ConfigError, _>(message.clone());
            let _ = e.value(py).setattr("message", message);
            e
        }
        CoreError::Network { message, status } => {
            let e = PyErr::new::<NetworkError, _>(message.clone());
            let v = e.value(py);
            let _ = v.setattr("message", message);
            let _ = v.setattr("status", status);
            e
        }
    })
    .expect("GIL should be held when converting ArtemisError in extension module")
}
