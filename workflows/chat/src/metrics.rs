//! Per-message metrics sidecar.
//!
//! Stored at `<state_dir>/metrics.json`, parallel to `transcript.json`.
//! `messages[i]` lines up with the underlying `lutin_llm::Message`
//! at the same index in the transcript; per-tool-call stats are keyed
//! by `tool_call.id` since positions can shift on edits.
//!
//! Kept out of `lutin_llm::Message` so the protocol/storage shapes of
//! the LLM crate stay clean — only the chat workflow renders these.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const METRICS_FILENAME: &str = "metrics.json";

pub fn metrics_path(state_dir: &Path) -> PathBuf {
    state_dir.join(METRICS_FILENAME)
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct MetricsSidecar {
    /// Parallel to the transcript's `Vec<Message>`.
    #[serde(default)]
    pub messages: Vec<StoredMeta>,
    /// Keyed by `ToolCall.id` so reordering doesn't break alignment.
    #[serde(default)]
    pub tools: HashMap<String, ToolStats>,
}

/// Metrics attached to one underlying `lutin_llm::Message`.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct StoredMeta {
    /// RFC3339 timestamp of when this message was added. Empty string
    /// when unknown (e.g. legacy transcripts loaded before metrics
    /// existed).
    #[serde(default)]
    pub timestamp: String,
    /// Set on `Message::Assistant` with non-empty text — the final
    /// assistant message of the turn.
    #[serde(default)]
    pub assistant: Option<AssistantStats>,
    /// Set on `Message::Assistant` with non-empty thinking.
    #[serde(default)]
    pub thinking: Option<AssistantStats>,
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
pub struct AssistantStats {
    pub ttft_ms: Option<u64>,
    pub duration_ms: Option<u64>,
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ToolStats {
    pub timestamp: String,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Error)]
pub enum MetricsError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("serialise: {0}")]
    Serialise(#[from] serde_json::Error),
}

pub fn load(state_dir: &Path) -> Result<MetricsSidecar, MetricsError> {
    let path = metrics_path(state_dir);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|source| MetricsError::Parse { path, source }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(MetricsSidecar::default()),
        Err(e) => Err(MetricsError::Io(e)),
    }
}

pub fn save(state_dir: &Path, sidecar: &MetricsSidecar) -> Result<(), MetricsError> {
    let final_path = metrics_path(state_dir);
    let tmp = state_dir.join("metrics.json.tmp");
    let body = serde_json::to_vec_pretty(sidecar)?;
    std::fs::write(&tmp, &body)?;
    std::fs::rename(&tmp, &final_path)?;
    Ok(())
}

pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}
