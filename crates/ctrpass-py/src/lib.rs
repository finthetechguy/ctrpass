use pyo3::prelude::*;

#[pymodule]
fn ctrpass(_m: &Bound<'_, PyModule>) -> PyResult<()> {
    Ok(())
}
