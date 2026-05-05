//! Generic per-session state IO.
//!
//! Every workflow lives at `<state_dir>/state.toml`. The schema is
//! workflow-specific (chat carries `persona`/`model_override`; a coding
//! workflow would carry different fields), so this module is generic
//! over `T: Serialize + DeserializeOwned`. Missing files deserialize to
//! `T::default()` — first-run sessions don't need to seed anything.

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;
use thiserror::Error;

const STATE_FILENAME: &str = "state.toml";

/// Resolve the canonical `<state_dir>/state.toml` path. Exposed so
/// callers can log it or watch the file with notify.
pub fn state_path(state_dir: &Path) -> PathBuf {
    state_dir.join(STATE_FILENAME)
}

#[derive(Debug, Error)]
pub enum StateError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("serialise: {0}")]
    Serialise(#[from] toml::ser::Error),
}

/// Load `T` from `<state_dir>/state.toml`. Returns `T::default()` if
/// the file is missing.
pub fn load<T: DeserializeOwned + Default>(state_dir: &Path) -> Result<T, StateError> {
    let path = state_path(state_dir);
    match std::fs::read_to_string(&path) {
        Ok(s) => toml::from_str(&s).map_err(|source| StateError::Parse { path, source }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(StateError::Io(e)),
    }
}

/// Persist `T` to `<state_dir>/state.toml`. Caller is responsible for
/// ensuring `state_dir` exists; the project supervisor creates session
/// dirs before spawning, so engines can rely on that.
pub fn save<T: Serialize>(state_dir: &Path, state: &T) -> Result<(), StateError> {
    let body = toml::to_string_pretty(state)?;
    std::fs::write(state_path(state_dir), body)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use tempfile::TempDir;

    #[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
    struct Sample {
        #[serde(default)]
        title: String,
        #[serde(default)]
        count: u32,
    }

    #[test]
    fn missing_returns_default() {
        let tmp = TempDir::new().unwrap();
        let s: Sample = load(tmp.path()).unwrap();
        assert_eq!(s, Sample::default());
    }

    #[test]
    fn roundtrip() {
        let tmp = TempDir::new().unwrap();
        let s = Sample {
            title: "hi".into(),
            count: 7,
        };
        save(tmp.path(), &s).unwrap();
        let loaded: Sample = load(tmp.path()).unwrap();
        assert_eq!(loaded, s);
    }

    #[test]
    fn parse_error_includes_path() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(state_path(tmp.path()), "this is :: not toml ::").unwrap();
        match load::<Sample>(tmp.path()) {
            Err(StateError::Parse { path, .. }) => {
                assert_eq!(path, state_path(tmp.path()));
            }
            other => panic!("{other:?}"),
        }
    }
}
