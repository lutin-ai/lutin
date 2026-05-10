//! Session-scoped image transcript persistence.
//!
//! Stored at `<state_dir>/transcript.json` as a `Vec<TranscriptEntry>`.
//! Atomic writes via tmp + rename so a crash mid-write leaves the
//! previous turn's file intact. Mirrors the chat workflow's
//! `transcript.json` shape conceptually, but the entry type is
//! image-specific (prompt + params + image refs rather than LLM
//! messages + token metrics).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use image_workflow::TranscriptEntry;

const TRANSCRIPT: &str = "transcript.json";
const TRANSCRIPT_TMP: &str = "transcript.json.tmp";

fn path(state_dir: &Path) -> PathBuf {
    state_dir.join(TRANSCRIPT)
}

/// Load the persisted transcript, or an empty vec if the file is
/// missing (first-run sessions).
pub fn load(state_dir: &Path) -> Result<Vec<TranscriptEntry>> {
    let p = path(state_dir);
    let bytes = match std::fs::read(&p) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("read {}", p.display())),
    };
    let entries: Vec<TranscriptEntry> = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse {}", p.display()))?;
    Ok(entries)
}

pub fn save(state_dir: &Path, entries: &[TranscriptEntry]) -> Result<()> {
    let p = path(state_dir);
    let tmp = state_dir.join(TRANSCRIPT_TMP);
    let body = serde_json::to_vec_pretty(entries).context("encode transcript")?;
    std::fs::write(&tmp, &body).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &p)
        .with_context(|| format!("rename {} -> {}", tmp.display(), p.display()))?;
    Ok(())
}
