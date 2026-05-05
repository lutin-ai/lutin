//! Tracks which files the model has read (and at what mtime) so that
//! `file_edit` / `file_write` can refuse stale or unread edits.
//!
//! Ported from the engine's `tool::read_state` with no behavioural change.
//! Keys are sandbox-relative `PathBuf`s derived from the caller-supplied
//! path via `canonicalize` where possible.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

#[derive(Debug)]
pub struct ReadState {
    root: PathBuf,
    inner: Mutex<HashMap<PathBuf, SystemTime>>,
}

impl ReadState {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Record that `path` was read at `mtime`.
    pub fn mark_read(&self, path: impl Into<PathBuf>, mtime: SystemTime) {
        let key = canonical_key(&path.into(), &self.root);
        self.inner.lock().unwrap().insert(key, mtime);
    }

    /// Returns the recorded mtime if the path has been read in this session.
    pub fn recorded_mtime(&self, path: &Path) -> Option<SystemTime> {
        let key = canonical_key(path, &self.root);
        self.inner.lock().unwrap().get(&key).copied()
    }

    /// Forget a path (e.g. after deletion).
    pub fn forget(&self, path: &Path) {
        let key = canonical_key(path, &self.root);
        self.inner.lock().unwrap().remove(&key);
    }
}

fn canonical_key(path: &Path, root: &Path) -> PathBuf {
    let resolved = canonicalize_best_effort(path);
    match resolved.strip_prefix(root) {
        Ok(rel) => rel.to_path_buf(),
        Err(_) => resolved,
    }
}

fn canonicalize_best_effort(path: &Path) -> PathBuf {
    if let Ok(c) = path.canonicalize() {
        return c;
    }
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
        if let Ok(cp) = parent.canonicalize() {
            return cp.join(name);
        }
    }
    path.to_path_buf()
}
