//! User-creatable entities stored under `.lutin/`.
//!
//! Three kinds for v1: personas, skills, variables. Each lives at a
//! conventional path inside the global or per-project `.lutin/` root,
//! discovered + loaded via [`lutin_storage::Resolver`]. The on-disk
//! filename (or directory name for skills) is the canonical identifier
//! — DTOs never restate the name field. Inter-entity references use
//! the same name string, not UUIDs.

pub mod persona;
pub mod skill;
pub mod variable;

pub use persona::{Persona, PersonaName, PersonaNameError, ReasoningEffort, ToolFilterMode};
pub use skill::{Skill, SkillEnvVar, SkillName, SkillNameError};
pub use variable::{Variables, VARIABLES_FILE};

use std::path::Path;

use lutin_storage::Scope;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EntityError {
    #[error("read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("parse {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("not found: {kind} {name}")]
    NotFound { kind: &'static str, name: String },
    #[error("invalid name: {0}")]
    InvalidName(String),
}

/// Read+deserialize a TOML file, mapping io/parse errors with the path
/// included for diagnostics.
fn read_toml<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, EntityError> {
    let text = std::fs::read_to_string(path).map_err(|source| EntityError::Io {
        path: path.display().to_string(),
        source,
    })?;
    toml::from_str(&text).map_err(|source| EntityError::Parse {
        path: path.display().to_string(),
        source,
    })
}

/// Read+deserialize a TOML file if it exists; `Ok(None)` when missing.
fn read_toml_if_exists<T: serde::de::DeserializeOwned>(
    path: &Path,
) -> Result<Option<T>, EntityError> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let parsed = toml::from_str(&text).map_err(|source| EntityError::Parse {
                path: path.display().to_string(),
                source,
            })?;
            Ok(Some(parsed))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(EntityError::Io {
            path: path.display().to_string(),
            source,
        }),
    }
}

/// One found-on-disk entity location with its scope.
#[derive(Debug, Clone)]
pub struct EntityLocation {
    pub scope: Scope,
    pub name: String,
    pub path: std::path::PathBuf,
}
