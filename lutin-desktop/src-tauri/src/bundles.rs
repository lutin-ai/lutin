//! Workflow plugin bundle cache.
//!
//! A "bundle" is a tar archive of a workflow's static plugin UI:
//! `lutin.workflow.json` at the root, then `index.html` and any
//! referenced assets. CP ships these via `GetWorkflowBundle`, keyed by
//! image digest. We cache the **unpacked** form on disk under the
//! Tauri app-cache dir so the URI scheme handler can serve files
//! directly without holding the bundle in memory.
//!
//! Layout: `<cache_root>/bundles/<workflow_id>/<digest_short>/...`.
//! `<digest_short>` is the docker image id with the `sha256:` prefix
//! stripped and truncated to 16 hex chars — long enough for collision
//! safety, short enough to keep paths sane.
//!
//! Concurrency: a single `Mutex<HashMap<…>>` in the cache holds an
//! entry per `(workflow_id, digest)`, populated lazily. Two concurrent
//! `ensure` calls for the same key race on the lock and the second
//! caller observes a present entry. A failed extraction leaves the
//! partial dir on disk; next call sees no entry and re-extracts on
//! top — `tar` overwrites file-by-file, so this is benign in practice.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;

use lutin_control_protocol::WorkflowId;

/// Unpacked bundle on disk, keyed by `(WorkflowId, digest)`.
#[derive(Default)]
pub struct BundleCache {
    /// Filesystem root holding `<workflow_id>/<digest_short>/` dirs.
    /// Set once at startup; `None` only in tests that bypass `init`.
    root: Mutex<Option<PathBuf>>,
    entries: Mutex<HashMap<(String, String), PathBuf>>,
}

impl BundleCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind the cache to a base directory. Idempotent — repeated calls
    /// with the same path are silently fine (Tauri's setup hook is the
    /// only caller).
    pub fn init(&self, base: PathBuf) -> io::Result<()> {
        let bundles = base.join("bundles");
        std::fs::create_dir_all(&bundles)?;
        *self.root.lock().expect("bundle root mutex poisoned") = Some(bundles);
        Ok(())
    }

    fn root(&self) -> io::Result<PathBuf> {
        self.root
            .lock()
            .expect("bundle root mutex poisoned")
            .clone()
            .ok_or_else(|| io::Error::other("bundle cache not initialized"))
    }

    /// Resolve the directory for a given `(workflow, digest)`. Returns
    /// `Some(dir)` if already cached on disk; `None` if a fetch+extract
    /// is needed.
    pub fn lookup(&self, workflow: &WorkflowId, digest: &str) -> Option<PathBuf> {
        let key = (workflow.as_str().to_owned(), digest.to_owned());
        if let Some(p) = self.entries.lock().expect("bundle map mutex poisoned").get(&key) {
            return Some(p.clone());
        }
        let dir = self.dir_for(workflow, digest).ok()?;
        if dir.join("lutin.workflow.json").is_file() {
            self.entries
                .lock()
                .expect("bundle map mutex poisoned")
                .insert(key, dir.clone());
            return Some(dir);
        }
        None
    }

    /// Extract a tarball into the cache. Caller has already fetched
    /// the bytes. Returns the directory the bundle was unpacked into.
    pub fn install(
        &self,
        workflow: &WorkflowId,
        digest: &str,
        tarball: &[u8],
    ) -> io::Result<PathBuf> {
        let dir = self.dir_for(workflow, digest)?;
        std::fs::create_dir_all(&dir)?;
        let mut archive = tar::Archive::new(tarball);
        // Disallow path traversal — any entry whose path escapes `dir`
        // is rejected. `tar`'s `unpack` already guards against this on
        // recent versions, but we double-check entry-by-entry so a
        // hostile bundle can't symlink its way out either.
        for entry in archive.entries()? {
            let mut entry = entry?;
            let rel = entry.path()?.into_owned();
            if rel.is_absolute() || rel.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
                return Err(io::Error::other(format!(
                    "bundle {}@{}: rejected unsafe path {}",
                    workflow.as_str(),
                    digest,
                    rel.display()
                )));
            }
            entry.unpack_in(&dir)?;
        }
        if !dir.join("lutin.workflow.json").is_file() {
            return Err(io::Error::other(format!(
                "bundle {}@{}: missing lutin.workflow.json at root",
                workflow.as_str(),
                digest
            )));
        }
        // Evict prior digests for this workflow id, both from the
        // in-memory map and on disk. `resolve_asset` looks up by
        // workflow id alone (the iframe URL doesn't carry a digest), so
        // leaving stale entries means asset reads after a rebuild can
        // randomly resolve to the old bundle.
        let mut map = self.entries.lock().expect("bundle map mutex poisoned");
        let stale_dirs: Vec<PathBuf> = map
            .iter()
            .filter_map(|((w, d), p)| {
                (w == workflow.as_str() && d != digest).then(|| p.clone())
            })
            .collect();
        map.retain(|(w, d), _| w != workflow.as_str() || d == digest);
        map.insert(
            (workflow.as_str().to_owned(), digest.to_owned()),
            dir.clone(),
        );
        drop(map);
        for stale in stale_dirs {
            if stale != dir {
                let _ = std::fs::remove_dir_all(&stale);
            }
        }
        Ok(dir)
    }

    /// Resolve a path inside a workflow's bundle dir, rejecting any
    /// traversal outside the bundle root. Returns the absolute path
    /// the URI scheme handler should read from.
    pub fn resolve_asset(&self, workflow: &WorkflowId, rel: &str) -> Option<PathBuf> {
        let map = self.entries.lock().expect("bundle map mutex poisoned");
        // Pick any digest for this workflow id. In practice we only
        // hold one at a time per workflow (cache-miss extracts the
        // current digest, evicting older ones is a future concern).
        let dir = map
            .iter()
            .find(|((w, _), _)| w == workflow.as_str())
            .map(|(_, p)| p.clone())?;
        drop(map);
        // Strip leading slashes so `Path::join` doesn't reset the root.
        let trimmed = rel.trim_start_matches('/');
        let candidate = dir.join(trimmed);
        let canon_dir = std::fs::canonicalize(&dir).ok()?;
        let canon_target = std::fs::canonicalize(&candidate).ok()?;
        if !canon_target.starts_with(&canon_dir) {
            return None;
        }
        Some(canon_target)
    }

    fn dir_for(&self, workflow: &WorkflowId, digest: &str) -> io::Result<PathBuf> {
        let root = self.root()?;
        Ok(root.join(workflow.as_str()).join(short_digest(digest)))
    }
}

/// Strip the `sha256:` prefix and truncate to 16 hex chars. Inputs
/// without the prefix pass through unchanged (also truncated).
fn short_digest(digest: &str) -> String {
    let stripped = digest.strip_prefix("sha256:").unwrap_or(digest);
    stripped.chars().take(16).collect()
}
