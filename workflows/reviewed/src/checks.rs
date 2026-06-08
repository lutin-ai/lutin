//! Post-edit build checks. Hardcoded to Rust for now.
//!
//! Two probes, both fail-soft (missing toolchain just logs and passes):
//!
//!   - **format** — `rustfmt` on the touched file right after a write/edit
//!     lands, before the working-set refresh re-reads it, so the model's
//!     view matches the formatted disk state. A formatter failure is a
//!     free syntax check — surfaced to the model as advisory text.
//!   - **check** — `cargo check` in the file's crate. Advisory per edit
//!     (intermediate states are allowed to be broken under the small-edits
//!     policy); blocking at turn end via the yield gate in `runtime.rs`.

use std::path::{Path, PathBuf};

use tokio::process::Command;
use tracing::warn;

const MAX_DIAG: usize = 4_000;

pub fn is_rust_file(path: &str) -> bool {
    Path::new(path).extension().is_some_and(|e| e == "rs")
}

/// Format the file in place. `None` = formatted (or rustfmt unavailable);
/// `Some(stderr)` = rustfmt refused, which almost always means syntax errors.
pub async fn format_file(root: &Path, path: &str) -> Option<String> {
    let file = resolve(root, path);
    let out = Command::new("rustfmt")
        .args(["--edition", "2024"])
        .arg(&file)
        .output()
        .await;
    match out {
        Ok(o) if o.status.success() => None,
        Ok(o) => Some(clip(String::from_utf8_lossy(&o.stderr).into_owned())),
        Err(e) => {
            warn!(error = %e, "checks: rustfmt unavailable; skipping format");
            None
        }
    }
}

/// Run `cargo check` in the crate containing `path`. `None` = pass (or no
/// manifest / no cargo — nothing to enforce); `Some(diagnostics)` = fail.
pub async fn cargo_check(root: &Path, path: &str) -> Option<String> {
    let file = resolve(root, path);
    let dir = manifest_dir(root, &file)?;
    let out = Command::new("cargo")
        .args(["check", "--quiet", "--message-format", "short"])
        .current_dir(&dir)
        .output()
        .await;
    match out {
        Ok(o) if o.status.success() => None,
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stderr).into_owned();
            if s.trim().is_empty() {
                s = String::from_utf8_lossy(&o.stdout).into_owned();
            }
            Some(clip(s))
        }
        Err(e) => {
            warn!(error = %e, "checks: cargo unavailable; skipping check");
            None
        }
    }
}

fn resolve(root: &Path, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() { p.to_path_buf() } else { root.join(p) }
}

fn manifest_dir(root: &Path, file: &Path) -> Option<PathBuf> {
    let mut dir = file.parent()?;
    loop {
        if dir.join("Cargo.toml").is_file() {
            return Some(dir.to_path_buf());
        }
        if dir == root {
            return None;
        }
        dir = dir.parent()?;
    }
}

fn clip(mut s: String) -> String {
    if s.len() > MAX_DIAG {
        s.truncate(MAX_DIAG);
        s.push_str("\n…[truncated]");
    }
    s
}
