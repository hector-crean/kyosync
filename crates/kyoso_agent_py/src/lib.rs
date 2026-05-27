//! PyO3 extension — the thinnest possible Python binding for the
//! kyoso agent surface.
//!
//! The whole strategy is **one dispatch seam**: Python gets a single
//! [`SceneAgent.dispatch(method, params)`] method that hands the
//! `(method, params)` pair through to
//! [`kyoso_agent_rpc::dispatch`]. The Python SDK (`kyoso-agent` on
//! PyPI) does the typing: Pydantic models marshal to/from the JSON
//! shape, and the agent verb names are spelled in idiomatic Python
//! (`agent.scan(...)`, `agent.match_(...)`) on top of this one method.
//!
//! Why not bind every verb 1:1 with PyO3? Two reasons:
//!
//! 1. The verb surface lives in `kyoso_agent`'s trait. Adding a verb
//!    would mean editing the trait, the RPC dispatcher, AND every
//!    language binding. With this seam, adding a verb only touches the
//!    first two — the Python SDK extends its `SceneAgent` class in
//!    Python and routes through `dispatch`.
//!
//! 2. PyO3 conversions for nested Pydantic-shaped types are non-trivial;
//!    `pythonize` already does Python ↔ `serde_json::Value` perfectly,
//!    and the JSON Schema → Pydantic codegen makes the typing work on
//!    the Python side for free.
//!
//! ## Build
//!
//! ```bash
//! cd crates/kyoso_agent_py
//! maturin develop          # builds + installs into the active venv
//! ```
//!
//! Or `maturin build --release` for a wheel.
//!
//! ## Threading
//!
//! `kyoso_agent::SceneAgent` owns a Bevy `App`, which is not `Send`.
//! The Python class is marked `#[pyclass(unsendable)]` so PyO3 enforces
//! "constructed and used on the same Python thread". Crossing threads
//! panics rather than corrupting world state.

use kyoso_agent::SceneAgent;
use kyoso_agent_rpc::{dispatch, DispatchError};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pythonize::{depythonize, pythonize};

/// Python-facing wrapper around the Rust [`SceneAgent`]. Constructed
/// with no args (mirrors `SceneAgent::new()` — `MinimalPlugins` +
/// `WatchPlugin`). Use the [`SceneAgentPy::dispatch`] method to call
/// any verb by name; the Python SDK's `SceneAgent` does the
/// per-verb typed wrapping.
#[pyclass(name = "SceneAgent", unsendable, module = "kyoso_agent_py")]
struct SceneAgentPy {
    inner: SceneAgent,
}

#[pymethods]
impl SceneAgentPy {
    #[new]
    fn new() -> Self {
        Self {
            inner: SceneAgent::new(),
        }
    }

    /// Route a verb call. `method` is the wire verb name (`"scan"`,
    /// `"match"`, `"move"`, …); `params` is a Python value (typically
    /// a `dict`) shaped to match the verb's params schema.
    ///
    /// Returns the verb's result as a Python value. Raises
    /// `ValueError` for malformed params or an unknown method, and
    /// `RuntimeError` for mutate failures (`MutateError`) or result
    /// serialization errors.
    fn dispatch<'py>(
        &mut self,
        py: Python<'py>,
        method: &str,
        params: Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        // Python value → serde_json::Value. `depythonize` accepts
        // `dict` / `list` / scalars / `None` and produces the canonical
        // JSON-shaped value the dispatcher expects.
        let params_value: serde_json::Value = depythonize(&params).map_err(|e| {
            PyValueError::new_err(format!("could not convert params to JSON: {e}"))
        })?;

        let result = dispatch(&mut self.inner, method, params_value)
            .map_err(dispatch_error_to_py)?;

        // serde_json::Value → Python. `pythonize` produces native
        // Python types (`dict`, `list`, `int`, `str`, `None`) that
        // Pydantic on the SDK side will validate.
        pythonize(py, &result)
            .map_err(|e| PyRuntimeError::new_err(format!("could not convert result to Python: {e}")))
    }
}

/// Map [`DispatchError`] onto the Python exception that best fits.
///
/// - `UnknownMethod` / `ParamsParse` → `ValueError` (caller bug).
/// - `Mutate` / `ResultSerialize` → `RuntimeError` (host/runtime issue).
fn dispatch_error_to_py(err: DispatchError) -> PyErr {
    match err {
        DispatchError::UnknownMethod(_) | DispatchError::ParamsParse { .. } => {
            PyValueError::new_err(err.to_string())
        }
        DispatchError::Mutate(_) | DispatchError::ResultSerialize { .. } => {
            PyRuntimeError::new_err(err.to_string())
        }
    }
}

/// The Python module entry point. The Python side imports it as
/// `kyoso_agent_py`; the extension contains exactly one class
/// (`SceneAgent`) — minimum surface, every other concern handled
/// either in the Rust `kyoso_agent_rpc` crate or the Python SDK.
#[pymodule]
fn kyoso_agent_py(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<SceneAgentPy>()?;
    Ok(())
}
