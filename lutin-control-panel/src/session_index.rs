//! Per-workflow on-disk session index.
//!
//! `<project>/.lutin/<workflow>/sessions.toml` lists every session
//! (running or dormant) CP knows about for that (project, workflow)
//! pair. CP appends to it on `StartSession`, removes from it on
//! `DeleteSession`, and reads it on `ListSessions` to enumerate
//! sessions even when no container is up.
//!
//! The schema is intentionally minimal: id + created_at. Anything
//! presentational (title, subtitle, etc.) is the workflow's
//! responsibility, written into `<project>/.lutin/sessions/<id>/summary.json`
//! while the engine is running. CP forwards that file's contents
//! verbatim — see `summary::read`.

use std::path::{Path, PathBuf};

use lutin_auth::{SessionId, Slug, WorkflowId};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml read: {0}")]
    TomlRead(#[from] toml::de::Error),
    #[error("toml write: {0}")]
    TomlWrite(#[from] toml::ser::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexEntry {
    pub id: SessionId,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct IndexFile {
    #[serde(default)]
    sessions: Vec<IndexEntry>,
}

/// `<project>/.lutin/<workflow>/sessions.toml`.
fn index_path(projects_root: &Path, slug: &Slug, workflow: &WorkflowId) -> PathBuf {
    projects_root
        .join(slug.as_str())
        .join(".lutin")
        .join(workflow.as_str())
        .join("sessions.toml")
}

fn load(path: &Path) -> Result<IndexFile, IndexError> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(toml::from_str(&s)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(IndexFile::default()),
        Err(e) => Err(e.into()),
    }
}

fn save(path: &Path, file: &IndexFile) -> Result<(), IndexError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = toml::to_string_pretty(file)?;
    // Atomic-ish: write to a sibling tmp file, rename. Avoids a torn
    // index file if the process is killed mid-write.
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, body.as_bytes())?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Append `(id, created_at)` to the workflow's index. No-op if an
/// entry with the same id already exists (idempotent on retries).
pub fn append(
    projects_root: &Path,
    slug: &Slug,
    workflow: &WorkflowId,
    id: &SessionId,
    created_at: &str,
) -> Result<(), IndexError> {
    let path = index_path(projects_root, slug, workflow);
    let mut file = load(&path)?;
    if file.sessions.iter().any(|e| e.id == *id) {
        return Ok(());
    }
    file.sessions.push(IndexEntry {
        id: id.clone(),
        created_at: created_at.to_owned(),
    });
    save(&path, &file)
}

/// Drop the entry for `id`. No-op if not present.
pub fn remove(
    projects_root: &Path,
    slug: &Slug,
    workflow: &WorkflowId,
    id: &SessionId,
) -> Result<(), IndexError> {
    let path = index_path(projects_root, slug, workflow);
    let mut file = load(&path)?;
    let before = file.sessions.len();
    file.sessions.retain(|e| e.id != *id);
    if file.sessions.len() == before {
        return Ok(());
    }
    save(&path, &file)
}

/// Every session known for `slug`, across every workflow. Used by
/// `ListSessions` — pairs each entry with its workflow id so the
/// caller can attach state + summary lookups.
pub fn read_all(projects_root: &Path, slug: &Slug) -> Vec<(WorkflowId, IndexEntry)> {
    let lutin_dir = projects_root.join(slug.as_str()).join(".lutin");
    let Ok(read) = std::fs::read_dir(&lutin_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for ent in read.flatten() {
        if !ent.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = ent.file_name();
        let Some(name) = name.to_str() else { continue };
        // Skip directories we know aren't workflow indexes:
        // `sessions/` (per-session state dirs), and any dotfile.
        if name == "sessions" || name.starts_with('.') {
            continue;
        }
        let Ok(workflow) = WorkflowId::parse(name) else {
            continue;
        };
        let path = ent.path().join("sessions.toml");
        let file = match load(&path) {
            Ok(f) => f,
            Err(_) => continue,
        };
        for entry in file.sessions {
            out.push((workflow.clone(), entry));
        }
    }
    out
}

/// Look up which workflow owns a given session id within a project.
/// `ResumeSession` uses this so the caller doesn't have to remember
/// the workflow alongside the session id.
pub fn find_workflow(
    projects_root: &Path,
    slug: &Slug,
    id: &SessionId,
) -> Option<WorkflowId> {
    read_all(projects_root, slug)
        .into_iter()
        .find(|(_, e)| e.id == *id)
        .map(|(wf, _)| wf)
}
