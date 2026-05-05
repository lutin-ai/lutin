//! Workflow discovery.
//!
//! A workflow is a standalone Cargo crate. The on-disk layout is:
//!
//! ```text
//! <root>/<workflow_id>/
//!   Cargo.toml         # package.name MUST equal <workflow_id>
//!   manifest.toml      # human-facing metadata (name, description)
//!   src/...
//!   target/<profile>/<workflow_id>   # compiled engine binary
//! ```
//!
//! The directory name is the canonical [`WorkflowId`] (also the Cargo
//! crate name and binary name — single source of truth). The manifest
//! holds only display fields; runtime config (model, system prompt,
//! provider) lives inside the workflow itself, sourced from personas
//! at runtime — not the project supervisor's concern.
//!
//! Subdirectories whose name does not parse as a `WorkflowId` are
//! skipped silently. A dir that *does* parse but lacks `manifest.toml`
//! is also skipped (work-in-progress, not yet a workflow). A malformed
//! manifest errors loudly.

use std::path::{Path, PathBuf};

use lutin_project_protocol::{WorkflowId, WorkflowInfo};
use serde::Deserialize;

/// Build profile used to locate compiled binaries and pass to
/// `cargo build`. Cargo only has two stable layouts (`target/debug`
/// for the dev profile, `target/release` for release); a richer enum
/// would need to track custom profile names against
/// `--profile <name>`, which we don't ship today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    Debug,
    Release,
}

impl Profile {
    pub const DEFAULT: Self = Self::Release;

    /// Subdirectory of `target/` where cargo writes the binary.
    pub fn dir_name(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Release => "release",
        }
    }

    /// Flag to pass to `cargo build`. `None` for debug (cargo's
    /// default), `Some("--release")` for release.
    pub fn cargo_flag(self) -> Option<&'static str> {
        match self {
            Self::Debug => None,
            Self::Release => Some("--release"),
        }
    }

    pub fn parse(s: &str) -> anyhow::Result<Self> {
        match s {
            "debug" => Ok(Self::Debug),
            "release" => Ok(Self::Release),
            other => Err(anyhow::anyhow!(
                "invalid build profile {other:?}: expected \"debug\" or \"release\""
            )),
        }
    }
}

#[derive(Debug, Deserialize)]
struct Manifest {
    name: String,
    #[serde(default)]
    description: Option<String>,
}

/// Full server-side workflow definition: public info plus the disk
/// path to the compiled engine binary.
#[derive(Debug, Clone)]
pub struct WorkflowDef {
    pub info: WorkflowInfo,
    /// Crate directory (parent of `Cargo.toml` and `manifest.toml`).
    pub crate_dir: PathBuf,
    pub profile: Profile,
}

impl WorkflowDef {
    /// Conventional path to the compiled engine binary.
    pub fn binary_path(&self) -> PathBuf {
        self.crate_dir
            .join("target")
            .join(self.profile.dir_name())
            .join(self.info.id.as_str())
    }
}

pub fn load_workflows(dir: &Path) -> anyhow::Result<Vec<WorkflowDef>> {
    let profile = match std::env::var("LUTIN_WORKFLOW_PROFILE") {
        Ok(s) => Profile::parse(&s)?,
        Err(_) => Profile::DEFAULT,
    };
    load_workflows_with_profile(dir, profile)
}

pub fn load_workflows_with_profile(dir: &Path, profile: Profile) -> anyhow::Result<Vec<WorkflowDef>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut workflows = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(id) = WorkflowId::parse(name) else {
            continue;
        };
        let manifest_path = path.join("manifest.toml");
        if !manifest_path.exists() {
            continue;
        }
        let bytes = std::fs::read_to_string(&manifest_path)?;
        let manifest: Manifest = toml::from_str(&bytes)
            .map_err(|e| anyhow::anyhow!("parse {}: {e}", manifest_path.display()))?;
        workflows.push(WorkflowDef {
            info: WorkflowInfo {
                id,
                name: manifest.name,
                description: manifest.description,
            },
            crate_dir: path,
            profile,
        });
    }
    workflows.sort_by(|a, b| a.info.id.as_str().cmp(b.info.id.as_str()));
    Ok(workflows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_manifest(root: &Path, id: &str, body: &str) {
        let dir = root.join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("manifest.toml"), body).unwrap();
    }

    #[test]
    fn missing_dir_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let v = load_workflows(&tmp.path().join("nope")).unwrap();
        assert!(v.is_empty());
    }

    #[test]
    fn loads_and_sorts_by_id() {
        let tmp = TempDir::new().unwrap();
        write_manifest(tmp.path(), "zeta", "name = \"Z\"\n");
        write_manifest(
            tmp.path(),
            "alpha",
            "name = \"A\"\ndescription = \"first\"\n",
        );
        let v = load_workflows(tmp.path()).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].info.id.as_str(), "alpha");
        assert_eq!(v[0].info.description.as_deref(), Some("first"));
        assert_eq!(v[1].info.id.as_str(), "zeta");
        assert_eq!(v[1].info.description, None);
    }

    #[test]
    fn binary_path_uses_profile() {
        let tmp = TempDir::new().unwrap();
        write_manifest(tmp.path(), "chat", "name = \"Chat\"\n");
        let v = load_workflows_with_profile(tmp.path(), Profile::Debug).unwrap();
        assert_eq!(v[0].binary_path(), tmp.path().join("chat/target/debug/chat"));
    }

    #[test]
    fn profile_parse_round_trip() {
        assert_eq!(Profile::parse("debug").unwrap(), Profile::Debug);
        assert_eq!(Profile::parse("release").unwrap(), Profile::Release);
        assert!(Profile::parse("Release").is_err());
        assert!(Profile::parse("").is_err());
    }

    #[test]
    fn skips_dirs_without_manifest() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join("scratch")).unwrap();
        write_manifest(tmp.path(), "real", "name = \"R\"\n");
        let v = load_workflows(tmp.path()).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].info.id.as_str(), "real");
    }

    #[test]
    fn skips_dirs_with_invalid_id() {
        let tmp = TempDir::new().unwrap();
        let bad = tmp.path().join(".git");
        fs::create_dir(&bad).unwrap();
        fs::write(bad.join("manifest.toml"), "name = \"x\"\n").unwrap();
        write_manifest(tmp.path(), "ok", "name = \"OK\"\n");
        let v = load_workflows(tmp.path()).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].info.id.as_str(), "ok");
    }

    #[test]
    fn malformed_manifest_errors() {
        let tmp = TempDir::new().unwrap();
        write_manifest(tmp.path(), "broken", "this is not = valid toml [[[");
        assert!(load_workflows(tmp.path()).is_err());
    }

    #[test]
    fn manifest_missing_name_errors() {
        let tmp = TempDir::new().unwrap();
        write_manifest(tmp.path(), "noname", "description = \"oops\"\n");
        assert!(load_workflows(tmp.path()).is_err());
    }
}
