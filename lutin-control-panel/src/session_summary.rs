//! Read-only access to the workflow-written `summary.json`.
//!
//! `<project>/.lutin/sessions/<id>/summary.json` is owned by the
//! workflow engine. CP reads it (when present) at `ListSessions`
//! time and passes the parsed `SessionSummary` through to the desktop
//! verbatim. Workflows that don't write the file get `None` and the
//! desktop falls back to a generic label.

use std::path::Path;

use lutin_auth::{SessionId, Slug};
use lutin_control_protocol::SessionSummary;

pub fn read(
    projects_root: &Path,
    slug: &Slug,
    session: &SessionId,
) -> Option<SessionSummary> {
    let path = projects_root
        .join(slug.as_str())
        .join(".lutin")
        .join("sessions")
        .join(session.as_str())
        .join("summary.json");
    let bytes = std::fs::read(&path).ok()?;
    serde_json::from_slice(&bytes).ok()
}
