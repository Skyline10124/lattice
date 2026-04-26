use pyo3::prelude::*;

pub mod types;

/// Artemis Core - Rust backend for the Artemis agent platform.
#[pymodule]
fn artemis_core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<types::Role>()?;
    m.add_class::<types::FunctionCall>()?;
    m.add_class::<types::ToolCall>()?;
    m.add_class::<types::Message>()?;
    m.add_class::<types::ToolDefinition>()?;
    m.add_class::<types::TransportType>()?;
    Ok(())
}
