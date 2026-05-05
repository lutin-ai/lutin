//! Per-session container orchestration.
//!
//! In the post-Phase-4 model CP launches workflow-session containers
//! directly: one container per active session. The container bind-
//! mounts the per-project tree at `/project` (RW) and the global config
//! tree at `/global` (RO), reads the same env handoff the legacy
//! lutin-project tier used (so the chat engine binary inside the image
//! is unchanged), and writes its bound addr to a handoff file under
//! `<projects_root>/<slug>/.lutin/sessions/<id>/handoff`.
//!
//! Session tokens are signed by the per-project keypair CP minted at
//! `CreateProject`. The desktop receives addr + token + project pubkey
//! and dials the container directly — CP doesn't proxy session traffic.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::time::{Duration, Instant};

use lutin_auth::{
    Scope, SessionId, SigningKey, Slug, Subject, Ttl, WorkflowId, mint_with_ttl,
    pubkey_to_string,
};
use lutin_control_protocol::{ProjectPubkey, SessionEndpoint, SessionInfo, WorkflowInfo};
use tokio::process::Command;
use tracing::{info, warn};

use crate::workflow_images;

/// How long to wait for the workflow container to publish its handoff.
const SPAWN_TIMEOUT: Duration = Duration::from_secs(15);
const SPAWN_POLL: Duration = Duration::from_millis(50);
/// Sessions tokens live for 1h; desktop refreshes via `OpenSession` on
/// reconnect. Same TTL the legacy project tier used.
const TOKEN_TTL: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("workflow not found: {0}")]
    WorkflowNotFound(WorkflowId),
    #[error("session not found: {0}")]
    SessionNotFound(SessionId),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("docker {op}: {detail}")]
    Docker { op: &'static str, detail: String },
    #[error("workflow container exited before publishing handoff")]
    ContainerExited,
    #[error("workflow container did not publish handoff within {0:?}")]
    HandoffTimeout(Duration),
    #[error("invalid handoff: {0}")]
    InvalidHandoff(String),
    #[error("auth: {0}")]
    Auth(#[from] lutin_auth::AuthError),
    #[error("rng: {0}")]
    Rng(#[from] getrandom::Error),
}

/// Live in-memory record of one running session container.
#[derive(Debug, Clone)]
pub struct RunningSession {
    pub info: SessionInfo,
    pub addr: SocketAddr,
    pub container_name: String,
    /// Cached so `OpenSession` doesn't re-mint the pubkey from disk.
    pub project_pubkey: ProjectPubkey,
}

/// `Slug → Vec<RunningSession>`. Held in the CP supervisor's task-local
/// state; not shared across threads.
pub type SessionRegistry = HashMap<Slug, Vec<RunningSession>>;

fn resolve_workflow_image(workflow: &WorkflowId) -> Result<String, SessionError> {
    // `docker image ls --filter label=<id>=<workflow>` returns repo:tag
    // for matching images. Pick the first — versioning + selection
    // policy is a future concern.
    let target = workflow.as_str();
    let out = std::process::Command::new("docker")
        .args([
            "image",
            "ls",
            "--filter",
            &format!("label=lutin.workflow.id={target}"),
            "--format",
            "{{.Repository}}:{{.Tag}}",
        ])
        .output()?;
    if !out.status.success() {
        return Err(SessionError::Docker {
            op: "image ls",
            detail: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        });
    }
    let image_ref = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .find(|s| !s.is_empty() && *s != "<none>:<none>")
        .map(str::to_owned);
    image_ref.ok_or_else(|| SessionError::WorkflowNotFound(workflow.clone()))
}

/// Enumerate installed workflow images. CP uses this to answer
/// `Request::ListWorkflows`. `name`/`description` are derived from the
/// image id today; richer metadata (e.g. via additional labels) is a
/// future polish.
pub fn list_workflows(global_config_dir: &Path) -> Vec<WorkflowInfo> {
    workflow_images::install_all(global_config_dir)
        .into_iter()
        .filter_map(|inst| {
            let id = WorkflowId::parse(&inst.id).ok()?;
            Some(WorkflowInfo {
                id,
                name: inst.id.clone(),
                description: None,
            })
        })
        .collect()
}

pub async fn start_session(
    registry: &mut SessionRegistry,
    slug: &Slug,
    workflow: &WorkflowId,
    projects_root: &Path,
    global_config_dir: &Path,
    container_prefix: &str,
) -> Result<(RunningSession, SessionEndpoint), SessionError> {
    let image_ref = resolve_workflow_image(workflow)?;
    let project_dir = projects_root.join(slug.as_str());
    let lutin_dir = project_dir.join(".lutin");
    let signing = lutin_keypair::load_or_create_keypair(&lutin_dir.join("keypair"))
        .map_err(|e| SessionError::Io(std::io::Error::other(e)))?;
    let project_pubkey = ProjectPubkey::new(signing.verifying_key().to_bytes());
    let project_pubkey_b64 = pubkey_to_string(&signing.verifying_key());

    let session_id = mint_session_id()?;
    let session_dir = lutin_dir.join("sessions").join(session_id.as_str());
    tokio::fs::create_dir_all(&session_dir).await?;
    let handoff_path = session_dir.join("handoff");
    if let Err(e) = tokio::fs::remove_file(&handoff_path).await
        && e.kind() != std::io::ErrorKind::NotFound
    {
        return Err(SessionError::Io(e));
    }

    let container_name = format!(
        "{container_prefix}-{slug}-{session}",
        slug = slug.as_str(),
        session = session_id.as_str()
    );
    // Best-effort cleanup of any stale container with the same name.
    let _ = Command::new("docker")
        .args(["rm", "-f", &container_name])
        .output()
        .await;

    // SAFETY: getuid/getgid are signal-safe and never fail.
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    let user_arg = format!("{uid}:{gid}");

    let project_mount = format!("{}:/project", project_dir.display());
    let global_mount = format!("{}:/global:ro", global_config_dir.display());
    let handoff_in_container = format!("/project/.lutin/sessions/{}/handoff", session_id.as_str());
    let state_in_container = format!("/project/.lutin/sessions/{}", session_id.as_str());

    info!(
        slug = %slug.as_str(),
        workflow = %workflow.as_str(),
        session = %session_id.as_str(),
        image = %image_ref,
        container = %container_name,
        "starting session container"
    );

    let out = Command::new("docker")
        .args([
            "run",
            "-d",
            "--rm",
            "--name",
            &container_name,
            // Host networking so the kernel-picked port the workflow
            // binds to is reachable from the desktop on host loopback —
            // mirrors the lutin-project tier's launch flow.
            "--network=host",
            "--user",
            &user_arg,
            "-v",
            &project_mount,
            "-v",
            &global_mount,
        ])
        .args([
            "-e",
            &format!("LUTIN_PROJECT_SLUG={}", slug.as_str()),
            "-e",
            &format!("LUTIN_PROJECT_PUBKEY={project_pubkey_b64}"),
            "-e",
            &format!("LUTIN_WORKFLOW_ID={}", workflow.as_str()),
            "-e",
            &format!("LUTIN_SESSION_ID={}", session_id.as_str()),
            "-e",
            &format!("LUTIN_SESSION_STATE_DIR={state_in_container}"),
            "-e",
            "LUTIN_WORKFLOW_ADDR=127.0.0.1:0",
            "-e",
            &format!("LUTIN_WORKFLOW_HANDOFF_PATH={handoff_in_container}"),
            "-e",
            "LUTIN_GLOBAL_CONFIG_DIR=/global",
            "-e",
            "LUTIN_PROJECT_CONFIG_DIR=/project/.lutin",
            &image_ref,
        ])
        .output()
        .await?;
    if !out.status.success() {
        return Err(SessionError::Docker {
            op: "run",
            detail: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        });
    }

    let addr = match poll_handoff_addr(&handoff_path, &container_name).await {
        Ok(a) => a,
        Err(e) => {
            // Best-effort teardown on failure; we don't want a half-spawned
            // container outliving the failed StartSession reply.
            let _ = Command::new("docker")
                .args(["rm", "-f", &container_name])
                .output()
                .await;
            return Err(e);
        }
    };

    let session_info = SessionInfo {
        id: session_id.clone(),
        workflow: workflow.clone(),
    };
    let token = mint_session_token(
        &signing,
        slug.clone(),
        workflow.clone(),
        session_id.clone(),
    )?;
    let endpoint = SessionEndpoint {
        addr,
        token,
        project_pubkey: project_pubkey.clone(),
    };
    let running = RunningSession {
        info: session_info,
        addr,
        container_name,
        project_pubkey,
    };
    registry
        .entry(slug.clone())
        .or_default()
        .push(running.clone());
    Ok((running, endpoint))
}

pub async fn stop_session(
    registry: &mut SessionRegistry,
    slug: &Slug,
    session: &SessionId,
) -> Result<(), SessionError> {
    let entry = registry.get_mut(slug).and_then(|sessions| {
        let idx = sessions.iter().position(|s| &s.info.id == session)?;
        Some(sessions.swap_remove(idx))
    });
    let entry = entry.ok_or_else(|| SessionError::SessionNotFound(session.clone()))?;
    let out = Command::new("docker")
        .args(["rm", "-f", &entry.container_name])
        .output()
        .await?;
    if !out.status.success() {
        // Container might already be gone; log but don't fail the call.
        warn!(
            container = %entry.container_name,
            stderr = %String::from_utf8_lossy(&out.stderr).trim(),
            "docker rm reported nonzero exit; treating as already-stopped"
        );
    }
    Ok(())
}

/// Re-mint a fresh token + endpoint for an already-running session,
/// e.g. when the desktop reconnects.
pub fn open_session(
    registry: &SessionRegistry,
    slug: &Slug,
    session: &SessionId,
    projects_root: &Path,
) -> Result<SessionEndpoint, SessionError> {
    let entry = registry
        .get(slug)
        .and_then(|s| s.iter().find(|s| &s.info.id == session))
        .ok_or_else(|| SessionError::SessionNotFound(session.clone()))?;
    let signing = lutin_keypair::load_or_create_keypair(
        &projects_root
            .join(slug.as_str())
            .join(".lutin")
            .join("keypair"),
    )
    .map_err(|e| SessionError::Io(std::io::Error::other(e)))?;
    let token = mint_session_token(
        &signing,
        slug.clone(),
        entry.info.workflow.clone(),
        entry.info.id.clone(),
    )?;
    Ok(SessionEndpoint {
        addr: entry.addr,
        token,
        project_pubkey: entry.project_pubkey.clone(),
    })
}

pub fn list_sessions(registry: &SessionRegistry, slug: &Slug) -> Vec<SessionInfo> {
    registry
        .get(slug)
        .map(|sessions| sessions.iter().map(|s| s.info.clone()).collect())
        .unwrap_or_default()
}

/// Tear down every session container for a given slug — used when the
/// project is deleted, and on supervisor shutdown.
pub async fn stop_all_for_slug(registry: &mut SessionRegistry, slug: &Slug) {
    if let Some(sessions) = registry.remove(slug) {
        for s in sessions {
            let _ = Command::new("docker")
                .args(["rm", "-f", &s.container_name])
                .output()
                .await;
        }
    }
}

pub async fn stop_all(registry: &mut SessionRegistry) {
    let slugs: Vec<Slug> = registry.keys().cloned().collect();
    for slug in slugs {
        stop_all_for_slug(registry, &slug).await;
    }
}

fn mint_session_token(
    signing: &SigningKey,
    project: Slug,
    workflow: WorkflowId,
    session: SessionId,
) -> Result<String, lutin_auth::AuthError> {
    mint_with_ttl(
        signing,
        Subject::parse("control-panel").expect("static subject is valid"),
        Scope::WorkflowSession {
            project,
            workflow,
            session,
        },
        Ttl::from_secs(TOKEN_TTL.as_secs()),
    )
}

/// 128-bit random session id, hex-encoded (32 chars). Same shape the
/// legacy lutin-project tier used so on-disk state under
/// `<projects_root>/<slug>/.lutin/sessions/<id>` keeps the same naming
/// convention across the cutover.
fn mint_session_id() -> Result<SessionId, getrandom::Error> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)?;
    let s: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(SessionId::parse(s).expect("hex32 is valid SessionId"))
}

async fn poll_handoff_addr(
    handoff_path: &Path,
    container_name: &str,
) -> Result<SocketAddr, SessionError> {
    let deadline = Instant::now() + SPAWN_TIMEOUT;
    loop {
        // Liveness check: if the container has already exited, don't
        // wait out the deadline. `docker inspect -f {{.State.Running}}`
        // returns "true"/"false" or errors if the container is gone.
        if !is_container_running(container_name).await {
            return Err(SessionError::ContainerExited);
        }
        match tokio::fs::read_to_string(handoff_path).await {
            Ok(s) if !s.trim().is_empty() => {
                let body = s.trim();
                return body.parse::<SocketAddr>().map_err(|e| {
                    SessionError::InvalidHandoff(format!("addr {body:?}: {e}"))
                });
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(SessionError::Io(e)),
        }
        if Instant::now() > deadline {
            return Err(SessionError::HandoffTimeout(SPAWN_TIMEOUT));
        }
        tokio::time::sleep(SPAWN_POLL).await;
    }
}

async fn is_container_running(name: &str) -> bool {
    let out = Command::new("docker")
        .args(["inspect", "-f", "{{.State.Running}}", name])
        .output()
        .await;
    match out {
        Ok(out) if out.status.success() => {
            String::from_utf8_lossy(&out.stdout).trim() == "true"
        }
        // No container by that name (or daemon error) — treat as "gone".
        _ => false,
    }
}

