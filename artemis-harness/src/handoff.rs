use std::path::Path;

// ---------------------------------------------------------------------------
// PythonHandoff — executes handoff.py scripts via PyO3
// ---------------------------------------------------------------------------

/// Execute a Python handoff function and return the next agent name.
pub fn run_python_handoff(
    script_path: &Path,
    output: &serde_json::Value,
    confidence: f64,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    use pyo3::types::PyAnyMethods;
    let script = std::fs::read_to_string(script_path)?;
    let output_json = serde_json::to_string(output)?;
    let code_cstr =
        std::ffi::CString::new(script).map_err(|e| format!("script contains null byte: {e}"))?;

    pyo3::Python::attach(|py| {
        let module = pyo3::types::PyModule::from_code(py, &code_cstr, c"handoff.py", c"handoff")?;

        let result = module.call_method1("should_handoff", (&output_json[..], confidence))?;

        if result.is_none() {
            Ok(None)
        } else {
            Ok(Some(result.extract::<String>()?))
        }
    })
}
