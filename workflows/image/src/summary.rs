//! Workflow-supplied `summary.json` written into the session state
//! dir so the desktop's session list can label this session. Schema
//! mirrors `lutin_control_protocol::SessionSummary`; we duplicate the
//! type rather than depend on the CP crate to keep the workflow's
//! dep footprint small.

use std::path::Path;

use image_workflow::{TranscriptEntry, TranscriptStatus};
use serde::Serialize;
use tracing::warn;

const TITLE_CHARS: usize = 80;
const PREVIEW_CHARS: usize = 160;

#[derive(Debug, Default, Serialize)]
struct ImageSummary {
    title: Option<String>,
    subtitle: Option<String>,
    last_activity: Option<String>,
    preview: Option<String>,
    persona: Option<String>,
    model: Option<String>,
    total_prompt_tokens: Option<u64>,
    total_completion_tokens: Option<u64>,
    context_tokens: Option<u32>,
    message_count: Option<u32>,
}

/// Best-effort: encode + atomic-write `<state_dir>/summary.json`.
/// Failures log a warning but don't bubble — a missing summary just
/// means the chrome shows a generic fallback label.
pub fn write(state_dir: &Path, entries: &[TranscriptEntry]) {
    let summary = build(entries);
    let body = match serde_json::to_vec_pretty(&summary) {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "encode image summary failed");
            return;
        }
    };
    let path = state_dir.join("summary.json");
    let tmp = state_dir.join("summary.json.tmp");
    if let Err(e) = std::fs::write(&tmp, &body) {
        warn!(error = %e, path = %tmp.display(), "write image summary tmp failed");
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        warn!(error = %e, "rename image summary tmp failed");
    }
}

fn build(entries: &[TranscriptEntry]) -> ImageSummary {
    let title = entries
        .iter()
        .find(|e| !e.prompt.trim().is_empty())
        .map(|e| truncate(e.prompt.trim(), TITLE_CHARS));
    let preview = entries
        .iter()
        .rev()
        .find(|e| !e.prompt.trim().is_empty())
        .map(|e| truncate(e.prompt.trim(), PREVIEW_CHARS));
    let image_count: u32 = entries
        .iter()
        .map(|e| match &e.status {
            TranscriptStatus::Done { images } => images.len() as u32,
            TranscriptStatus::Error { .. } => 0,
        })
        .sum();
    let subtitle = if image_count > 0 {
        Some(format!(
            "{image_count} image{}",
            if image_count == 1 { "" } else { "s" }
        ))
    } else {
        None
    };
    let last_activity = entries.last().map(|e| e.started_at.clone());
    ImageSummary {
        title,
        subtitle,
        last_activity,
        preview,
        message_count: Some(entries.len() as u32),
        ..Default::default()
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i >= max_chars {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}
