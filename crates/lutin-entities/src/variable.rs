//! Variables: a flat key-value file substituted into prompts via
//! `%var:name%` placeholders.
//!
//! Stored as a single `.lutin/variables.toml` per tier. Two-tier load
//! merges *per-key*: project keys override global keys. (Unlike
//! settings subtables, which override as a unit, variables are
//! conceptually flat and each key is independently managed.)

use std::collections::BTreeMap;
use std::path::Path;

use lutin_storage::Resolver;
use serde::{Deserialize, Serialize};

use crate::{read_toml_if_exists, EntityError};

pub const VARIABLES_FILE: &str = "variables.toml";

/// Flat string-to-string map. `BTreeMap` keeps iteration deterministic
/// for diagnostics; toml round-trips both directions.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct Variables(pub BTreeMap<String, String>);

impl Variables {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    /// Load merged variables: global first, project overrides per key.
    pub fn load(resolver: &Resolver) -> Result<Self, EntityError> {
        let mut merged = BTreeMap::new();
        for (_scope, path) in resolver.all_files(Path::new(VARIABLES_FILE)) {
            if let Some(layer) = read_toml_if_exists::<Variables>(&path)? {
                merged.extend(layer.0);
            }
        }
        Ok(Variables(merged))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn missing_returns_empty() {
        let dir = tempdir().unwrap();
        let resolver = Resolver::new(dir.path(), None::<&Path>);
        let v = Variables::load(&resolver).unwrap();
        assert!(v.0.is_empty());
    }

    #[test]
    fn project_overrides_per_key() {
        let dir = tempdir().unwrap();
        let global = dir.path().join("global");
        let project = dir.path().join("project");
        write(
            &global.join("variables.toml"),
            r#"display_name = "User"
default_model = "global-model"
"#,
        );
        write(
            &project.join("variables.toml"),
            r#"default_model = "project-model"
extra = "yes"
"#,
        );
        let resolver = Resolver::new(&global, Some(&project));
        let v = Variables::load(&resolver).unwrap();
        assert_eq!(v.get("display_name"), Some("User"));
        assert_eq!(v.get("default_model"), Some("project-model"));
        assert_eq!(v.get("extra"), Some("yes"));
    }
}
