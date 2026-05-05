//! Per-session subprocess supervision.
//!
//! Workflows are standalone Cargo crates. On `StartSession` the project
//! supervisor spawns the workflow's compiled engine binary as a child
//! process and lets it own everything substantive: WS endpoint, persona
//! selection, LLM provider construction, session state. Project-side
//! responsibility is reduced to:
//!
//! 1. Spawn the binary with the env handoff (config dirs, slug, ids,
//!    issuer pubkey, addr=`127.0.0.1:0`, handoff path).
//! 2. Wait for the workflow to publish its bound addr to the handoff
//!    file (mirrors the control-panel→project pattern).
//! 3. Mint `Scope::WorkflowSession` tokens for clients on demand.
//! 4. Kill the child on `StopSession` or supervisor teardown.
//!
//! No in-process LLM, no broadcast bus, no session protocol on this
//! side. Clients connect to the workflow directly using the addr +
//! token returned by `OpenSession`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lutin_auth::{Scope, SigningKey, Slug, Subject, Ttl, mint_with_ttl};
use lutin_project_protocol::{SessionId, WorkflowId};
use tokio::process::{Child, Command};
use tracing::warn;

use crate::workflows::WorkflowDef;

/// How long to wait for a workflow binary to publish its handoff file.
const SPAWN_TIMEOUT: Duration = Duration::from_secs(15);
const SPAWN_POLL: Duration = Duration::from_millis(25);

#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("workflow binary not found at {path}: {source}")]
    BinaryMissing { path: String, source: std::io::Error },
    #[error("workflow exited before publishing handoff: status={0}")]
    ChildExited(std::process::ExitStatus),
    #[error("workflow did not publish handoff within {0:?}")]
    HandoffTimeout(Duration),
    #[error("invalid handoff: {0}")]
    InvalidHandoff(String),
}

/// Owning handle for one running session subprocess.
pub struct SessionHandle {
    pub addr: SocketAddr,
    child: Child,
}

impl SessionHandle {
    /// Mint a `WorkflowSession`-scoped token for clients connecting to
    /// this session's WS endpoint. The workflow binary verifies it
    /// against the project pubkey passed in via env at spawn.
    pub fn mint_token(
        &self,
        signing: &SigningKey,
        project: Slug,
        workflow: WorkflowId,
        session: SessionId,
    ) -> Result<String, lutin_auth::AuthError> {
        mint_with_ttl(
            signing,
            Subject::parse("project-supervisor").expect("static subject is valid"),
            Scope::WorkflowSession {
                project,
                workflow,
                session,
            },
            Ttl::from_secs(60 * 60),
        )
    }

    /// Kill the child process and reap it. Tolerates an already-dead
    /// child (`kill_on_drop` may have signaled it earlier).
    pub async fn stop(mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }

    /// Test-only constructor. Spawns a trivial child so tests that
    /// only exercise `mint_token` don't need a real workflow binary.
    #[cfg(test)]
    pub fn for_test(addr: SocketAddr) -> Self {
        let child = Command::new("sleep")
            .arg("3600")
            .kill_on_drop(true)
            .spawn()
            .expect("spawn sleep for test");
        Self { addr, child }
    }
}

/// Spawn the workflow binary as a child and wait for it to publish its
/// bound addr. The project's signing key signs WorkflowSession tokens;
/// the *issuer pubkey* the workflow verifies against is the project's
/// own pubkey, passed via env so the workflow doesn't need to load it
/// from disk.
pub async fn spawn_session(
    project: &Slug,
    workflow: &WorkflowId,
    session: &SessionId,
    def: &WorkflowDef,
    project_pubkey_b64: &str,
    global_config_dir: &Path,
    project_config_dir: &Path,
    session_dir: &Path,
) -> Result<SessionHandle, SpawnError> {
    tokio::fs::create_dir_all(session_dir).await?;
    // Handoff lives inside the per-session dir; the dir itself names
    // the session, so the file is just "handoff".
    let handoff_path = session_dir.join("handoff");
    // A stale handoff from a prior run with the same session id would
    // let us read the previous addr. Removing tolerantly: NotFound is
    // fine, anything else propagates.
    match tokio::fs::remove_file(&handoff_path).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(SpawnError::Io(e)),
    }

    let binary_path = def.binary_path();
    let mut cmd = Command::new(&binary_path);
    cmd.env("LUTIN_PROJECT_SLUG", project.as_str())
        .env("LUTIN_PROJECT_PUBKEY", project_pubkey_b64)
        .env("LUTIN_WORKFLOW_ID", workflow.as_str())
        .env("LUTIN_SESSION_ID", session.as_str())
        .env("LUTIN_GLOBAL_CONFIG_DIR", global_config_dir)
        .env("LUTIN_PROJECT_CONFIG_DIR", project_config_dir)
        .env("LUTIN_SESSION_STATE_DIR", session_dir)
        .env("LUTIN_WORKFLOW_ADDR", "127.0.0.1:0")
        .env("LUTIN_WORKFLOW_HANDOFF_PATH", &handoff_path)
        .kill_on_drop(true);

    let mut child = cmd.spawn().map_err(|source| SpawnError::BinaryMissing {
        path: binary_path.display().to_string(),
        source,
    })?;

    // Poll for the handoff file. The workflow writes one line: the
    // bound socket addr. If the child exits first, surface the exit
    // status — debugging "no handoff" is easier when we already know
    // the binary died.
    let deadline = Instant::now() + SPAWN_TIMEOUT;
    let body = loop {
        if let Some(status) = child.try_wait()? {
            return Err(SpawnError::ChildExited(status));
        }
        match tokio::fs::read_to_string(&handoff_path).await {
            Ok(s) if !s.is_empty() => break s,
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                let _ = child.kill().await;
                return Err(SpawnError::Io(e));
            }
        }
        if Instant::now() > deadline {
            let _ = child.kill().await;
            return Err(SpawnError::HandoffTimeout(SPAWN_TIMEOUT));
        }
        tokio::time::sleep(SPAWN_POLL).await;
    };

    let addr_str = body.trim();
    let addr: SocketAddr = addr_str.parse().map_err(|e| {
        warn!(handoff = %handoff_path.display(), body = %body, "malformed workflow handoff");
        SpawnError::InvalidHandoff(format!("addr {addr_str:?}: {e}"))
    })?;

    Ok(SessionHandle { addr, child })
}

// State paths are owned by the supervisor; these helpers compute the
// per-project layout so callers don't have to import `lutin_storage`
// just for path math.
//
// `default_sessions_root` is the parent dir holding every session's
// own subdir. `default_session_dir` is the per-session subdir (also
// where the workflow's handoff file lives).
pub fn default_sessions_root(project_config_dir: &Path) -> PathBuf {
    project_config_dir.join("sessions")
}

pub fn default_session_dir(project_config_dir: &Path, session: &SessionId) -> PathBuf {
    default_sessions_root(project_config_dir).join(session.as_str())
}
