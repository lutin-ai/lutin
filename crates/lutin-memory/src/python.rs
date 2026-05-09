use std::sync::{Arc, OnceLock};

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use crate::Result;
use crate::store::Store;

/// Modules we try to preload into every script's globals. Stdlib entries are
/// effectively always available; numpy / pandas / networkx are probed once and
/// silently skipped if the host Python doesn't have them.
const CANDIDATE_MODULES: &[&str] = &[
    "datetime",
    "json",
    "re",
    "math",
    "statistics",
    "collections",
    "itertools",
    "functools",
    "numpy",
    "pandas",
    "networkx",
];

/// Returns the subset of [`CANDIDATE_MODULES`] that the local Python interpreter
/// can actually import. Probed once and cached.
pub fn available_modules() -> &'static [&'static str] {
    static CACHE: OnceLock<Vec<&'static str>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Python::attach(|py| {
            CANDIDATE_MODULES
                .iter()
                .copied()
                .filter(|m| py.import(*m).is_ok())
                .collect()
        })
    })
}

/// Run a Python script with a `memory` object exposing the store.
///
/// Available API in script:
///   memory.query(sql) -> list[dict]
///   memory.get(id)    -> dict | None
///
/// Captures stdout and returns it as a string.
pub fn run_script(store: Arc<Store>, code: &str) -> Result<String> {
    Python::attach(|py| -> PyResult<String> {
        let sys = py.import("sys")?;
        let io = py.import("io")?;
        let buf = io.call_method0("StringIO")?;
        sys.setattr("stdout", &buf)?;

        let memory = Py::new(py, MemoryHandle { store })?;
        let globals = PyDict::new(py);
        globals.set_item("memory", memory)?;
        // Preload common modules — each script runs in a fresh globals dict, so
        // without this the model has to `import datetime` every call (and often
        // forgets, producing a spurious NameError mid-loop).
        for name in available_modules() {
            if let Ok(m) = py.import(*name) {
                globals.set_item(*name, m)?;
            }
        }

        let res = py.run(&std::ffi::CString::new(code).unwrap(), Some(&globals), None);

        let out: String = buf.call_method0("getvalue")?.extract()?;
        res?;
        Ok(out)
    })
    .map_err(|e| crate::Error::Python(format!("{e}")))
}

#[pyclass]
struct MemoryHandle {
    store: Arc<Store>,
}

#[pymethods]
impl MemoryHandle {
    #[pyo3(signature = (sql, params=None))]
    fn query<'py>(
        &self,
        py: Python<'py>,
        sql: &str,
        params: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyList>> {
        let bound: Vec<serde_json::Value> = match params {
            None => Vec::new(),
            Some(obj) => py_to_json_list(&obj)?,
        };
        let rows = self
            .store
            .query_sql_with_params(sql, &bound)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{e}")))?;
        let list = PyList::empty(py);
        for r in rows {
            let d = PyDict::new(py);
            for (k, v) in r {
                d.set_item(k, json_to_py(py, &v)?)?;
            }
            list.append(d)?;
        }
        Ok(list)
    }

    fn get<'py>(&self, py: Python<'py>, id: i64) -> PyResult<Option<Bound<'py, PyDict>>> {
        let ev = self
            .store
            .get_event(id)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{e}")))?;
        let Some(ev) = ev else { return Ok(None) };
        let v = serde_json::to_value(&ev)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{e}")))?;
        let json_to_py_val = json_to_py(py, &v)?;
        Ok(Some(json_to_py_val.cast_into::<PyDict>()?))
    }
}

fn py_to_json_list<'py>(obj: &Bound<'py, PyAny>) -> PyResult<Vec<serde_json::Value>> {
    if obj.is_none() {
        return Ok(Vec::new());
    }
    if let Ok(list) = obj.cast::<PyList>() {
        let mut out = Vec::with_capacity(list.len());
        for item in list.iter() {
            out.push(py_to_json(&item)?);
        }
        return Ok(out);
    }
    if let Ok(tup) = obj.cast::<pyo3::types::PyTuple>() {
        let mut out = Vec::with_capacity(tup.len());
        for item in tup.iter() {
            out.push(py_to_json(&item)?);
        }
        return Ok(out);
    }
    Err(pyo3::exceptions::PyTypeError::new_err(
        "params must be a list or tuple",
    ))
}

fn py_to_json(obj: &Bound<'_, PyAny>) -> PyResult<serde_json::Value> {
    if obj.is_none() {
        return Ok(serde_json::Value::Null);
    }
    if let Ok(b) = obj.extract::<bool>() {
        return Ok(serde_json::Value::Bool(b));
    }
    if let Ok(i) = obj.extract::<i64>() {
        return Ok(serde_json::Value::from(i));
    }
    if let Ok(f) = obj.extract::<f64>() {
        return Ok(serde_json::Number::from_f64(f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null));
    }
    if let Ok(s) = obj.extract::<String>() {
        return Ok(serde_json::Value::String(s));
    }
    Err(pyo3::exceptions::PyTypeError::new_err(format!(
        "unsupported param type: {}",
        obj.get_type().name()?,
    )))
}

fn json_to_py<'py>(py: Python<'py>, v: &serde_json::Value) -> PyResult<Bound<'py, PyAny>> {
    use serde_json::Value;
    Ok(match v {
        Value::Null => py.None().into_bound(py),
        Value::Bool(b) => b.into_pyobject(py)?.to_owned().into_any(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.into_pyobject(py)?.into_any()
            } else if let Some(f) = n.as_f64() {
                f.into_pyobject(py)?.into_any()
            } else {
                py.None().into_bound(py)
            }
        }
        Value::String(s) => s.into_pyobject(py)?.into_any(),
        Value::Array(a) => {
            let list = PyList::empty(py);
            for item in a {
                list.append(json_to_py(py, item)?)?;
            }
            list.into_any()
        }
        Value::Object(o) => {
            let d = PyDict::new(py);
            for (k, val) in o {
                d.set_item(k, json_to_py(py, val)?)?;
            }
            d.into_any()
        }
    })
}
