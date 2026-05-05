//! On-disk project registry. Single TOML file at
//! `<data_dir>/projects.toml`; the supervisor reads it on boot and
//! rewrites it after every Create/Delete.

use std::io;
use std::path::{Path, PathBuf};

use lutin_control_protocol::{DisplayName, ProjectInfo, Slug};
use serde::{Deserialize, Serialize};

use super::ProjectRecord;

const FILE_NAME: &str = "projects.toml";

#[derive(Debug, Serialize, Deserialize, Default)]
struct File {
    #[serde(default, rename = "project")]
    projects: Vec<Entry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Entry {
    slug: Slug,
    display_name: DisplayName,
}

fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE_NAME)
}

pub fn load(data_dir: &Path) -> io::Result<Vec<ProjectRecord>> {
    let p = path(data_dir);
    let text = match std::fs::read_to_string(&p) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let file: File = toml::from_str(&text)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e}", p.display())))?;
    file.projects
        .into_iter()
        .map(|e| {
            let keypair_path = data_dir.join(e.slug.as_str()).join(".lutin").join("keypair");
            let signing = lutin_keypair::load_or_create_keypair(&keypair_path)
                .map_err(io::Error::other)?;
            Ok(ProjectRecord {
                info: ProjectInfo {
                    slug: e.slug,
                    display_name: e.display_name,
                },
                signing,
            })
        })
        .collect()
}

pub fn save(data_dir: &Path, projects: &[ProjectRecord]) -> io::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let file = File {
        projects: projects
            .iter()
            .map(|r| Entry {
                slug: r.info.slug.clone(),
                display_name: r.info.display_name.clone(),
            })
            .collect(),
    };
    let text = toml::to_string_pretty(&file)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    let final_path = path(data_dir);
    let tmp_path = data_dir.join(format!(".{FILE_NAME}.tmp"));
    std::fs::write(&tmp_path, text)?;
    std::fs::rename(&tmp_path, &final_path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lutin_control_protocol::{DisplayName, Slug};
    use tempfile::TempDir;

    fn record(slug: &str) -> ProjectRecord {
        ProjectRecord {
            info: ProjectInfo {
                slug: Slug::parse(slug).unwrap(),
                display_name: DisplayName::parse(slug).unwrap(),
            },
            signing: lutin_auth::generate_keypair().unwrap(),
        }
    }

    #[test]
    fn missing_file_returns_empty() {
        let tmp = TempDir::new().unwrap();
        assert!(load(tmp.path()).unwrap().is_empty());
    }

    #[test]
    fn roundtrip() {
        let tmp = TempDir::new().unwrap();
        // Pre-create the keypair file `load` expects to find for each entry.
        let lutin_dir = tmp.path().join("alpha").join(".lutin");
        std::fs::create_dir_all(&lutin_dir).unwrap();
        lutin_keypair::load_or_create_keypair(&lutin_dir.join("keypair")).unwrap();

        save(tmp.path(), &[record("alpha")]).unwrap();
        let loaded = load(tmp.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].info.slug.as_str(), "alpha");
    }

    /// Backward-compat: registries with a `limits` field still load
    /// (toml just ignores unknown fields).
    #[test]
    fn legacy_limits_field_ignored() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join(FILE_NAME),
            "[[project]]\nslug = \"old\"\ndisplay_name = \"Old\"\nlimits = { memory = \"1g\" }\n",
        )
        .unwrap();
        let lutin_dir = tmp.path().join("old").join(".lutin");
        std::fs::create_dir_all(&lutin_dir).unwrap();
        lutin_keypair::load_or_create_keypair(&lutin_dir.join("keypair")).unwrap();

        let loaded = load(tmp.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].info.slug.as_str(), "old");
    }
}
