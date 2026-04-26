pub mod provider;
pub mod streaming;
pub mod types;

use pyo3::prelude::*;

/// Add two integers together.
#[pyfunction]
fn add(a: i64, b: i64) -> i64 {
    a + b
}

/// Artemis Core - Rust backend for the Artemis agent platform.
#[pymodule]
fn artemis_core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(add, m)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add() {
        assert_eq!(add(2, 3), 5);
        assert_eq!(add(-1, 1), 0);
        assert_eq!(add(0, 0), 0);
        assert_eq!(add(100, 200), 300);
    }
}
