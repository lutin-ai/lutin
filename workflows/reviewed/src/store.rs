use std::path::{Path, PathBuf};

use lutin_llm::Message;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::types::Agent;

const FILENAME: &str = "state.json";

#[derive(Debug, Serialize, Deserialize)]
pub struct SavedState {
    pub messages: Vec<Message>,
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("io {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parse {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("serialise: {0}")]
    Serialise(#[from] serde_json::Error),
}

pub fn state_path(state_dir: &Path) -> PathBuf {
    state_dir.join(FILENAME)
}

pub fn save(agent: &Agent) -> Result<(), StoreError> {
    let snapshot = SavedState {
        messages: agent.messages.clone(),
    };
    let final_path = state_path(&agent.state_dir);
    let tmp = agent.state_dir.join(format!("{FILENAME}.tmp"));
    std::fs::create_dir_all(&agent.state_dir).map_err(|source| StoreError::Io {
        path: agent.state_dir.clone(),
        source,
    })?;
    let body = serde_json::to_vec_pretty(&snapshot)?;
    std::fs::write(&tmp, &body).map_err(|source| StoreError::Io {
        path: tmp.clone(),
        source,
    })?;
    std::fs::rename(&tmp, &final_path).map_err(|source| StoreError::Io {
        path: final_path,
        source,
    })?;
    Ok(())
}

pub fn load(state_dir: &Path) -> Result<Option<SavedState>, StoreError> {
    let path = state_path(state_dir);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(StoreError::Io { path, source }),
    };
    let saved: SavedState = serde_json::from_slice(&bytes)
        .map_err(|source| StoreError::Parse { path, source })?;
    Ok(Some(saved))
}
