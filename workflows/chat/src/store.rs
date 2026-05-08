//! Single-source-of-truth chat transcript store.
//!
//! Each conversation entry pairs an `lutin_llm::Message` with the
//! per-message `MessageMetrics` (timestamp, TTFT, duration, tokens,
//! and per-tool-call stats). Mutations move both halves together so
//! the two can never drift — the parallel-vec sidecar approach is
//! gone.
//!
//! Persisted at `<state_dir>/transcript.json` as JSON for
//! debuggability. Atomic writes via tmp-file + rename so a crash
//! mid-write leaves the previous turn's file intact.
//!
//! On load, accepts either the new `Vec<Entry>` shape or the legacy
//! `Vec<Message>` shape (transcripts written before metrics existed)
//! and migrates the legacy shape to entries with empty metrics. The
//! migration is one-way; the next save writes the new format.

use std::path::{Path, PathBuf};

use lutin_llm::Message;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const TRANSCRIPT_FILENAME: &str = "transcript.json";

pub fn transcript_path(state_dir: &Path) -> PathBuf {
    state_dir.join(TRANSCRIPT_FILENAME)
}

/// One row of conversation: the LLM-facing message plus everything the
/// chat UI needs to render its footer. Single struct → mutations on
/// `Vec<Entry>` keep the two in lockstep automatically.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub message: Message,
    /// Metrics default to "all-None / empty" when absent in the file
    /// so legacy `Vec<Message>` transcripts round-trip cleanly.
    #[serde(default)]
    pub metrics: MessageMetrics,
}

/// Metrics for one `Entry`.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct MessageMetrics {
    /// RFC3339 timestamp of when this entry was added. `None` for
    /// entries that came in before metrics existed.
    #[serde(default)]
    pub timestamp: Option<String>,
    /// Stats for the assistant's text body (only set on
    /// `Message::Assistant` with non-empty `text`).
    #[serde(default)]
    pub text: Option<TextStats>,
    /// Stats for the assistant's reasoning/thinking block (only set
    /// on `Message::Assistant` with non-empty `thinking`).
    #[serde(default)]
    pub thinking: Option<ThinkingStats>,
    /// Aligned 1:1 with `Message::Assistant.tool_calls`. Empty for
    /// every other message kind. Aligned positionally so a `tool_call`
    /// rename or reorder will not be possible without also touching
    /// this vec — they move as a pair.
    #[serde(default)]
    pub tools: Vec<ToolStats>,
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
pub struct TextStats {
    pub ttft_ms: Option<u64>,
    pub duration_ms: Option<u64>,
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
}

/// Reasoning/thinking has no token split because providers either don't
/// expose per-block thinking-token counts or fold them into the
/// completion total.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
pub struct ThinkingStats {
    pub ttft_ms: Option<u64>,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ToolStats {
    pub timestamp: Option<String>,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Error)]
pub enum StoreError {
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

/// Load the conversation. Returns an empty vec if the file is missing
/// (first-run sessions). Falls back to legacy `Vec<Message>` shape if
/// the new shape doesn't parse; legacy entries get default metrics.
pub fn load(state_dir: &Path) -> Result<Vec<Entry>, StoreError> {
    let path = transcript_path(state_dir);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(StoreError::Io(e)),
    };
    if let Ok(entries) = serde_json::from_slice::<Vec<Entry>>(&bytes) {
        return Ok(entries);
    }
    let legacy: Vec<Message> = serde_json::from_slice(&bytes)
        .map_err(|source| StoreError::Parse { path, source })?;
    Ok(legacy
        .into_iter()
        .map(|message| Entry { message, metrics: MessageMetrics::default() })
        .collect())
}

pub fn save(state_dir: &Path, entries: &[Entry]) -> Result<(), StoreError> {
    let final_path = transcript_path(state_dir);
    let tmp = state_dir.join("transcript.json.tmp");
    let body = serde_json::to_vec_pretty(entries)?;
    std::fs::write(&tmp, &body)?;
    std::fs::rename(&tmp, &final_path)?;
    Ok(())
}

pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Convenience accessor — drops the metrics half so legacy callers
/// that only need the message list (agent seeding, summary build) get
/// the same shape they did before.
pub fn messages(entries: &[Entry]) -> Vec<Message> {
    entries.iter().map(|e| e.message.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn missing_returns_empty() {
        let tmp = TempDir::new().unwrap();
        assert!(load(tmp.path()).unwrap().is_empty());
    }

    #[test]
    fn legacy_vec_message_loads_with_default_metrics() {
        let tmp = TempDir::new().unwrap();
        let legacy = vec![
            Message::User("hi".into()),
            Message::Assistant {
                text: "hey".into(),
                thinking: None,
                tool_calls: Vec::new(),
            },
        ];
        let body = serde_json::to_vec_pretty(&legacy).unwrap();
        std::fs::write(transcript_path(tmp.path()), &body).unwrap();
        let entries = load(tmp.path()).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries[0].metrics.timestamp.is_none());
        match &entries[0].message {
            Message::User(s) => assert_eq!(s, "hi"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn entry_roundtrip_preserves_metrics() {
        let tmp = TempDir::new().unwrap();
        let entries = vec![Entry {
            message: Message::User("hi".into()),
            metrics: MessageMetrics {
                timestamp: Some("2026-05-08T12:00:00Z".into()),
                ..Default::default()
            },
        }];
        save(tmp.path(), &entries).unwrap();
        let loaded = load(tmp.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded[0].metrics.timestamp.as_deref(),
            Some("2026-05-08T12:00:00Z"),
        );
    }
}
