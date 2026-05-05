//! Cargo-build-on-StartSession.
//!
//! Workflows are standalone Cargo crates owned by the user; the
//! supervisor's job before spawning is to make sure the engine binary
//! exists and is up to date relative to the source. Cargo already does
//! its own incremental rebuild check, but invoking it costs ~hundreds
//! of ms per StartSession. We do a cheap mtime sweep first and skip
//! cargo entirely on the warm path; the cold path runs `cargo build`
//! in the crate dir and streams stdout/stderr lines as broadcast
//! events so a UI can render progress.
//!
//! Bookend events (`WorkflowBuildStarted` / `WorkflowBuildFinished`)
//! are emitted by the supervisor *call site*, not from inside this
//! module — keeps the lifecycle effects in one place and lets the
//! caller see whether a build ran at all (the warm path returns
//! `BuildOutcome::Skipped` and emits no events).

use std::path::Path;
use std::process::Stdio;
use std::time::SystemTime;

use lutin_project_protocol::{Event, SessionId, WorkflowId};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::broadcast;
use tracing::warn;

use crate::workflows::WorkflowDef;

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Cheap freshness check: binary present, and no source file newer
/// than the binary's mtime. Walks `Cargo.toml` and `src/` only —
/// we don't try to track transitive crate sources, since cargo would
/// catch that on the rare case it matters. Caller decides whether
/// to invoke `run_cargo` based on the result; we expose this rather
/// than a combined `ensure_built` so the caller can emit
/// `WorkflowBuildStarted` *before* output starts streaming.
pub fn is_fresh(def: &WorkflowDef) -> std::io::Result<bool> {
    let bin = def.binary_path();
    let bin_mtime = match std::fs::metadata(&bin) {
        Ok(m) => m.modified()?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    let cargo_toml = def.crate_dir.join("Cargo.toml");
    match std::fs::metadata(&cargo_toml) {
        Ok(m) if m.modified()? > bin_mtime => return Ok(false),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let src_dir = def.crate_dir.join("src");
    if src_dir.exists() && tree_has_newer(&src_dir, bin_mtime)? {
        return Ok(false);
    }
    Ok(true)
}

/// Iterative directory walk — workflow `src/` trees can in principle
/// be arbitrarily deep, so we use an explicit stack rather than
/// recursing. Symlinks are followed via `metadata`; cycle detection
/// is not implemented (workflows are user-owned source trees, not
/// adversarial input).
fn tree_has_newer(root: &Path, baseline: SystemTime) -> std::io::Result<bool> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let ft = entry.file_type()?;
            if ft.is_dir() {
                stack.push(entry.path());
            } else if ft.is_file() && entry.metadata()?.modified()? > baseline {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Run `cargo build` in the workflow crate dir, streaming each output
/// line as `Event::WorkflowBuildOutput`. Returns the exit code (`None`
/// if killed by signal). Doesn't emit `Started`/`Finished` bookends —
/// that's the caller's responsibility.
pub async fn run_cargo(
    def: &WorkflowDef,
    session: &SessionId,
    events: &broadcast::Sender<Event>,
) -> Result<Option<i32>, BuildError> {
    let mut cmd = Command::new("cargo");
    cmd.arg("build");
    if let Some(flag) = def.profile.cargo_flag() {
        cmd.arg(flag);
    }
    cmd.current_dir(&def.crate_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd.spawn()?;
    let stdout = child.stdout.take().expect("piped by Stdio::piped above");
    let stderr = child.stderr.take().expect("piped by Stdio::piped above");

    let stdout_task = spawn_line_pump(stdout, session.clone(), def.info.id.clone(), events.clone());
    let stderr_task = spawn_line_pump(stderr, session.clone(), def.info.id.clone(), events.clone());

    let status = child.wait().await?;
    // Wait for the pumps to drain remaining buffered lines so the
    // caller's `WorkflowBuildFinished` never races ahead of the last
    // Output event. A pump panic is a bug in this module — surface it.
    stdout_task.await.expect("stdout pump panicked");
    stderr_task.await.expect("stderr pump panicked");

    Ok(status.code())
}

fn spawn_line_pump<R>(
    reader: R,
    session: SessionId,
    workflow: WorkflowId,
    events: broadcast::Sender<Event>,
) -> tokio::task::JoinHandle<()>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    let _ = events.send(Event::WorkflowBuildOutput {
                        session: session.clone(),
                        workflow: workflow.clone(),
                        line,
                    });
                }
                Ok(None) => break,
                Err(e) => {
                    // Pipe IO errors during a build are rare (process
                    // closed pipe abnormally). Log and stop — the
                    // child.wait() in run_cargo will still report the
                    // process exit, which is the load-bearing signal.
                    warn!(error = %e, "build output pipe closed with error");
                    break;
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::Profile;
    use lutin_project_protocol::{WorkflowId, WorkflowInfo};
    use std::fs;
    use std::time::Duration;
    use tempfile::TempDir;

    fn def_in(tmp: &TempDir) -> WorkflowDef {
        WorkflowDef {
            info: WorkflowInfo {
                id: WorkflowId::parse("wf").unwrap(),
                name: "wf".into(),
                description: None,
            },
            crate_dir: tmp.path().to_path_buf(),
            profile: Profile::Debug,
        }
    }

    /// Write sources *first*, sleep to clear sub-second mtime
    /// resolution, then write the binary — so the binary's mtime is
    /// strictly later than every source's.
    fn write_with_old_sources(tmp: &TempDir) {
        fs::write(tmp.path().join("Cargo.toml"), "").unwrap();
        fs::create_dir_all(tmp.path().join("src")).unwrap();
        fs::write(tmp.path().join("src/lib.rs"), "").unwrap();
        // Filesystems vary; 1.2s clears second-granularity mtimes.
        std::thread::sleep(Duration::from_millis(1200));
        let target = tmp.path().join("target/debug");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("wf"), b"x").unwrap();
    }

    #[test]
    fn missing_binary_is_stale() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join("src")).unwrap();
        fs::write(tmp.path().join("Cargo.toml"), "").unwrap();
        assert!(!is_fresh(&def_in(&tmp)).unwrap());
    }

    #[test]
    fn binary_newer_than_sources_is_fresh() {
        let tmp = TempDir::new().unwrap();
        write_with_old_sources(&tmp);
        assert!(is_fresh(&def_in(&tmp)).unwrap());
    }

    #[test]
    fn newer_source_file_marks_stale() {
        let tmp = TempDir::new().unwrap();
        write_with_old_sources(&tmp);
        std::thread::sleep(Duration::from_millis(1200));
        fs::write(tmp.path().join("src/lib.rs"), "changed").unwrap();
        assert!(!is_fresh(&def_in(&tmp)).unwrap());
    }

    #[test]
    fn nested_source_file_marks_stale() {
        let tmp = TempDir::new().unwrap();
        write_with_old_sources(&tmp);
        std::thread::sleep(Duration::from_millis(1200));
        let nested = tmp.path().join("src/sub/deep");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("mod.rs"), "x").unwrap();
        assert!(!is_fresh(&def_in(&tmp)).unwrap());
    }

}
