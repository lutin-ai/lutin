use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use crate::read_state::ReadState;

/// Context shared by every tool built in this crate.
pub struct ToolContext {
    /// Sandbox root. All filesystem paths are jailed below this directory.
    pub root: PathBuf,
    /// Extra env vars to inject into subprocesses spawned by `shell`.
    pub env: Arc<[(String, String)]>,
    /// Shared HTTP client used by `http_request` and `web_search`.
    pub http: reqwest::Client,
    /// Per-session record of files that have been read. Drives the
    /// read-before-edit guard in `file_edit` / `file_write`.
    pub read_state: Arc<ReadState>,
}

impl ToolContext {
    /// Resolve a user-supplied path to an absolute path under `root`.
    pub fn resolve(&self, user_path: &str) -> PathBuf {
        let p = Path::new(user_path);
        if p.is_absolute() {
            if let Ok(stripped) = p.strip_prefix(&self.root) {
                return self.root.join(stripped);
            }
        }
        self.root.join(normalize_relative(user_path))
    }
}

fn normalize_relative(user_path: &str) -> PathBuf {
    let path = Path::new(user_path);
    let stripped = if path.is_absolute() {
        path.strip_prefix("/").unwrap_or(path)
    } else {
        path
    };

    let mut parts: Vec<&std::ffi::OsStr> = Vec::new();
    for component in stripped.components() {
        match component {
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(s) => parts.push(s),
        }
    }
    parts.iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_relative() {
        let ctx = ToolContext {
            root: PathBuf::from("/root"),
            env: Arc::from([]),
            http: reqwest::Client::new(),
            read_state: Arc::new(ReadState::new(PathBuf::from("/root"))),
        };
        assert_eq!(
            ctx.resolve("src/main.rs"),
            PathBuf::from("/root/src/main.rs")
        );
        assert_eq!(
            ctx.resolve("/etc/passwd"),
            PathBuf::from("/root/etc/passwd")
        );
        assert_eq!(ctx.resolve("../../bad"), PathBuf::from("/root/bad"));
    }
}
