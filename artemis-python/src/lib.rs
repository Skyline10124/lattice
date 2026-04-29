mod engine;
mod errors;

use pyo3::prelude::*;

#[pymodule]
fn artemis_core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", "0.1.0")?;

    // Register exception hierarchy
    m.add("ArtemisError", m.py().get_type::<errors::ArtemisError>())?;
    m.add(
        "RateLimitError",
        m.py().get_type::<errors::RateLimitError>(),
    )?;
    m.add(
        "AuthenticationError",
        m.py().get_type::<errors::AuthenticationError>(),
    )?;
    m.add(
        "ModelNotFoundError",
        m.py().get_type::<errors::ModelNotFoundError>(),
    )?;
    m.add(
        "ProviderUnavailableError",
        m.py().get_type::<errors::ProviderUnavailableError>(),
    )?;
    m.add(
        "ContextWindowExceededError",
        m.py().get_type::<errors::ContextWindowExceededError>(),
    )?;
    m.add(
        "ToolExecutionError",
        m.py().get_type::<errors::ToolExecutionError>(),
    )?;
    m.add(
        "StreamingError",
        m.py().get_type::<errors::StreamingError>(),
    )?;
    m.add("ConfigError", m.py().get_type::<errors::ConfigError>())?;
    m.add("NetworkError", m.py().get_type::<errors::NetworkError>())?;

    // Register engine types
    m.add_class::<engine::ArtemisEngine>()?;
    m.add_class::<engine::PyResolvedModel>()?;

    Ok(())
}
