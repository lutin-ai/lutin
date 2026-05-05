//! On-disk project registry. Single TOML file at
//! `<data_dir>/projects.toml`; the supervisor reads it on boot and
//! rewrites it after every Create/Delete. Status is intentionally
//! omitted — it's runtime state recomputed from the live `running`
//! list, never restored from disk.

use std::io;
use std::path::{Path, PathBuf};

use lutin_control_protocol::{DisplayName, ProjectInfo, ProjectStatus, Slug};
use serde::{Deserialize, Serialize};

use super::{ProjectLimits, ProjectRecord};

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
    /// Per-project resource caps for the Docker backend. Operator-
    /// editable today; absent → uncapped. Fields default to None
    /// individually, so a partial `[project.limits]` table is fine.
    #[serde(default, skip_serializing_if = "limits_is_default")]
    limits: ProjectLimits,
}

fn limits_is_default(l: &ProjectLimits) -> bool {
    l == &ProjectLimits::default()
}

fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE_NAME)
}

/// Read and parse the registry. Missing file is not an error: a fresh
/// install has no projects yet. Parse errors propagate so the operator
/// notices a corrupted registry instead of us silently wiping it on
/// the next save.
pub fn load(data_dir: &Path) -> io::Result<Vec<ProjectRecord>> {
    let p = path(data_dir);
    let text = match std::fs::read_to_string(&p) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let file: File = toml::from_str(&text)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e}", p.display())))?;
    Ok(file
        .projects
        .into_iter()
        .map(|e| ProjectRecord {
            info: ProjectInfo {
                slug: e.slug,
                display_name: e.display_name,
                status: ProjectStatus::Stopped,
            },
            limits: e.limits,
        })
        .collect())
}

/// Atomic write: serialize → temp file in same dir → rename. The
/// rename is atomic on POSIX, so a crash mid-write can't leave a
/// half-written registry that fails to parse on next boot.
pub fn save(data_dir: &Path, projects: &[ProjectRecord]) -> io::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let file = File {
        projects: projects
            .iter()
            .map(|r| Entry {
                slug: r.info.slug.clone(),
                display_name: r.info.display_name.clone(),
                limits: r.limits.clone(),
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

    fn record(slug: &str, limits: ProjectLimits) -> ProjectRecord {
        ProjectRecord {
            info: ProjectInfo {
                slug: Slug::parse(slug).unwrap(),
                display_name: DisplayName::parse(slug).unwrap(),
                status: ProjectStatus::Stopped,
            },
            limits,
        }
    }

    /// A fresh data dir has no registry — we treat that as "no
    /// projects" rather than an error so first boot just works.
    #[test]
    fn missing_file_returns_empty() {
        let tmp = TempDir::new().unwrap();
        assert!(load(tmp.path()).unwrap().is_empty());
    }

    /// Default (uncapped) limits should not be serialised — keeps the
    /// on-disk file clean for the common case and avoids noisy diffs.
    #[test]
    fn default_limits_omitted_from_disk() {
        let tmp = TempDir::new().unwrap();
        save(tmp.path(), &[record("alpha", ProjectLimits::default())]).unwrap();
        let text = std::fs::read_to_string(tmp.path().join("projects.toml")).unwrap();
        assert!(
            !text.contains("limits"),
            "expected no `limits` key on disk, got:\n{text}"
        );
        let loaded = load(tmp.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].limits, ProjectLimits::default());
    }

    /// Set limits roundtrip through save/load with all fields preserved.
    #[test]
    fn populated_limits_roundtrip() {
        use super::super::{CpuQuota, MemorySize};
        let tmp = TempDir::new().unwrap();
        let limits = ProjectLimits {
            memory: Some(MemorySize::parse("2g").unwrap()),
            cpus: Some(CpuQuota::parse("1.5").unwrap()),
            pids: Some(std::num::NonZeroU32::new(256).unwrap()),
        };
        save(tmp.path(), &[record("capped", limits.clone())]).unwrap();
        let loaded = load(tmp.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].limits, limits);
    }

    /// Backward-compat: registries written before `limits` existed
    /// must keep loading without error, with default limits filled in.
    #[test]
    fn pre_limits_toml_loads_with_defaults() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join(FILE_NAME),
            "[[project]]\nslug = \"old\"\ndisplay_name = \"Old\"\n",
        )
        .unwrap();
        let loaded = load(tmp.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].info.slug.as_str(), "old");
        assert_eq!(loaded[0].limits, ProjectLimits::default());
    }

    /// Operator-edited TOML with an inline `limits` table should parse.
    /// This is the primary UX for setting limits today.
    #[test]
    fn operator_edited_inline_limits_loads() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join(FILE_NAME),
            "[[project]]\n\
             slug = \"demo\"\n\
             display_name = \"Demo\"\n\
             limits = { memory = \"1g\", cpus = \"0.5\", pids = 128 }\n",
        )
        .unwrap();
        let loaded = load(tmp.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded[0].limits.memory.as_ref().map(|m| m.as_str()),
            Some("1g")
        );
        assert_eq!(
            loaded[0].limits.cpus.as_ref().map(|c| c.as_str()),
            Some("0.5"),
        );
        assert_eq!(loaded[0].limits.pids.map(|p| p.get()), Some(128));
    }

    /// Partial limits (only some fields set) should leave the others
    /// at None, not error.
    #[test]
    fn partial_limits_loads() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join(FILE_NAME),
            "[[project]]\n\
             slug = \"part\"\n\
             display_name = \"Part\"\n\
             [project.limits]\n\
             memory = \"512m\"\n",
        )
        .unwrap();
        let loaded = load(tmp.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded[0].limits.memory.as_ref().map(|m| m.as_str()),
            Some("512m")
        );
        assert_eq!(loaded[0].limits.cpus, None);
        assert_eq!(loaded[0].limits.pids, None);
    }

    /// Bad memory string (no digits) is rejected at load, not at
    /// `docker run` time. The parse error is surfaced through serde
    /// so the file path and field are reported.
    #[test]
    fn bad_memory_fails_at_load() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join(FILE_NAME),
            "[[project]]\n\
             slug = \"bad\"\n\
             display_name = \"Bad\"\n\
             [project.limits]\n\
             memory = \"junk\"\n",
        )
        .unwrap();
        let err = load(tmp.path()).unwrap_err();
        assert!(
            err.to_string().contains("invalid memory"),
            "expected invalid-memory message, got: {err}"
        );
    }

    /// Zero pids would make the container unable to fork — rejected
    /// by `NonZeroU32`'s deserialize, not by docker.
    #[test]
    fn zero_pids_fails_at_load() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join(FILE_NAME),
            "[[project]]\n\
             slug = \"zero\"\n\
             display_name = \"Zero\"\n\
             [project.limits]\n\
             pids = 0\n",
        )
        .unwrap();
        assert!(load(tmp.path()).is_err());
    }

    /// Negative cpus would round-trip as a string but parse as a
    /// non-positive f64; rejected at load.
    #[test]
    fn non_positive_cpus_fails_at_load() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join(FILE_NAME),
            "[[project]]\n\
             slug = \"neg\"\n\
             display_name = \"Neg\"\n\
             [project.limits]\n\
             cpus = \"-1\"\n",
        )
        .unwrap();
        let err = load(tmp.path()).unwrap_err();
        assert!(
            err.to_string().contains("invalid cpus"),
            "expected invalid-cpus message, got: {err}"
        );
    }
}
