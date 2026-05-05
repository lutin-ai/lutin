//! Per-session message transcript persistence.
//!
//! Stores the full `Vec<lutin_llm::Message>` for one chat session as
//! JSON at `<state_dir>/transcript.json`. JSON over postcard so the
//! file is debuggable with `cat`; the per-turn rewrite cost is
//! negligible for chat-sized transcripts.
//!
//! Atomic writes via tempfile + rename so a crash mid-write can never
//! leave a torn file — the previous turn's transcript stays valid.
//!
//! Workflows that don't run an agent loop don't need this module;
//! workflows that do (chat, future coding/research workflows) all want
//! the same shape.

use std::path::{Path, PathBuf};

use lutin_llm::Message;
use thiserror::Error;

const TRANSCRIPT_FILENAME: &str = "transcript.json";

/// Resolve the canonical `<state_dir>/transcript.json` path.
pub fn transcript_path(state_dir: &Path) -> PathBuf {
    state_dir.join(TRANSCRIPT_FILENAME)
}

#[derive(Debug, Error)]
pub enum TranscriptError {
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

/// Load the message list from `<state_dir>/transcript.json`. Returns
/// an empty vec if the file is missing — first-run sessions don't need
/// to seed the file.
pub fn load(state_dir: &Path) -> Result<Vec<Message>, TranscriptError> {
    let path = transcript_path(state_dir);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|source| TranscriptError::Parse { path, source }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(TranscriptError::Io(e)),
    }
}

/// Persist the message list to `<state_dir>/transcript.json` atomically
/// (write to temp, rename over). Caller is responsible for ensuring
/// `state_dir` exists.
pub fn save(state_dir: &Path, messages: &[Message]) -> Result<(), TranscriptError> {
    let final_path = transcript_path(state_dir);
    let body = serde_json::to_vec_pretty(messages)?;

    // tempfile + persist gives us atomic rename on the same filesystem.
    // If the workflow crashes mid-write, the previous turn's file is
    // unchanged — readers never see a torn JSON document.
    let tmp = tempfile::NamedTempFile::new_in(state_dir)?;
    std::fs::write(tmp.path(), &body)?;
    tmp.persist(&final_path)
        .map_err(|e| TranscriptError::Io(e.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lutin_llm::Message;
    use tempfile::TempDir;

    #[test]
    fn missing_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let m = load(tmp.path()).unwrap();
        assert!(m.is_empty());
    }

    #[test]
    fn roundtrip_user_and_assistant() {
        let tmp = TempDir::new().unwrap();
        let messages = vec![
            Message::User("hello".into()),
            Message::Assistant {
                text: "hi there".into(),
                tool_calls: Vec::new(),
                thinking: None,
            },
        ];
        save(tmp.path(), &messages).unwrap();
        let loaded = load(tmp.path()).unwrap();
        assert_eq!(loaded.len(), 2);
        match &loaded[0] {
            Message::User(s) => assert_eq!(s, "hello"),
            other => panic!("{other:?}"),
        }
        match &loaded[1] {
            Message::Assistant { text, .. } => assert_eq!(text, "hi there"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn save_overwrites() {
        let tmp = TempDir::new().unwrap();
        save(tmp.path(), &[Message::User("first".into())]).unwrap();
        save(tmp.path(), &[Message::User("second".into())]).unwrap();
        let loaded = load(tmp.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        match &loaded[0] {
            Message::User(s) => assert_eq!(s, "second"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_error_includes_path() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(transcript_path(tmp.path()), "not json").unwrap();
        match load(tmp.path()) {
            Err(TranscriptError::Parse { path, .. }) => {
                assert_eq!(path, transcript_path(tmp.path()));
            }
            other => panic!("{other:?}"),
        }
    }
}
