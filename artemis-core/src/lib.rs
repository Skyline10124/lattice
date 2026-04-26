pub mod engine;
pub mod errors;
pub mod mock;
pub mod provider;
pub mod streaming;
pub mod transport;
pub mod types;

use errors::py_exc;
use pyo3::prelude::*;

/// Artemis Core - Rust backend for the Artemis agent platform.
#[pymodule]
fn artemis_core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // ── Register Python exception hierarchy ──────────────────────────
    m.add("ArtemisError", m.py().get_type::<py_exc::ArtemisError>())?;
    m.add("RateLimitError", m.py().get_type::<py_exc::RateLimitError>())?;
    m.add(
        "AuthenticationError",
        m.py().get_type::<py_exc::AuthenticationError>(),
    )?;
    m.add(
        "ModelNotFoundError",
        m.py().get_type::<py_exc::ModelNotFoundError>(),
    )?;
    m.add(
        "ProviderUnavailableError",
        m.py().get_type::<py_exc::ProviderUnavailableError>(),
    )?;
    m.add(
        "ContextWindowExceededError",
        m.py().get_type::<py_exc::ContextWindowExceededError>(),
    )?;
    m.add(
        "ToolExecutionError",
        m.py().get_type::<py_exc::ToolExecutionError>(),
    )?;
    m.add("StreamingError", m.py().get_type::<py_exc::StreamingError>())?;
    m.add("ConfigError", m.py().get_type::<py_exc::ConfigError>())?;
    m.add("NetworkError", m.py().get_type::<py_exc::NetworkError>())?;

    // ── Register types ───────────────────────────────────────────────
    m.add_class::<types::Role>()?;
    m.add_class::<types::FunctionCall>()?;
    m.add_class::<types::ToolCall>()?;
    m.add_class::<types::Message>()?;
    m.add_class::<types::ToolDefinition>()?;
    m.add_class::<types::TransportType>()?;

    // ── Register engine types ────────────────────────────────────────
    m.add_class::<engine::ArtemisEngine>()?;
    m.add_class::<engine::Event>()?;
    m.add_class::<engine::ToolCallInfo>()?;

    Ok(())
}
