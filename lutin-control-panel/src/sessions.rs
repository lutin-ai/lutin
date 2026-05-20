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

use std::net::SocketAddr;
use std::path::Path;
use std::time::{Duration, Instant};

use lutin_auth::{
    Scope, SessionId, SigningKey, Slug, Subject, Ttl, WorkflowId, mint_with_ttl,
    pubkey_to_string,
};
use lutin_control_protocol::{
    ApiError, ProjectPubkey, SessionEndpoint, SessionInfo, SessionState, WorkflowInfo,
};
use tokio::process::Command;
use tracing::{info, warn};

use crate::session_index::{self, IndexError};
use crate::session_summary;
use crate::workflow_images;

/// How long to wait for the workflow container to publish its handoff.
const SPAWN_TIMEOUT: Duration = Duration::from_secs(15);
const SPAWN_POLL: Duration = Duration::from_millis(50);
/// Session tokens live for 90 days; desktop refreshes via `OpenSession` on reconnect.
const TOKEN_TTL: Duration = Duration::from_secs(60 * 60 * 24 * 90);
/// Prefix for `docker run --name`; full name is `{PREFIX}-{slug}-{session}`.
const CONTAINER_PREFIX: &str = "lutin-session";
/// Docker labels stamped on every session container so the supervisor
/// can rediscover them after a CP restart. Slug + session id are
/// non-trivial to round-trip out of the container name (slugs may
/// contain dashes), so we don't parse the name — we read the labels.
const SESSION_LABEL_SLUG: &str = "lutin.session.slug";
const SESSION_LABEL_WORKFLOW: &str = "lutin.session.workflow";
const SESSION_LABEL_ID: &str = "lutin.session.id";

impl From<SessionError> for ApiError {
    fn from(e: SessionError) -> Self {
        match e {
            SessionError::WorkflowNotFound(id) => ApiError::WorkflowNotFound(id),
            SessionError::SessionNotFound(id) => ApiError::SessionNotFound(id),
            other => ApiError::Supervisor(other.to_string()),
        }
    }
}

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
    #[error("session index: {0}")]
    Index(#[from] IndexError),
}

/// Live in-memory record of one running session container.
#[derive(Debug, Clone)]
pub struct RunningSession {
    pub slug: Slug,
    pub info: SessionInfo,
    pub addr: SocketAddr,
    pub container_name: String,
    /// Cached so `OpenSession` doesn't re-mint the pubkey from disk.
    pub project_pubkey: ProjectPubkey,
}

/// Flat list of every running session across all projects. Held in the
/// CP supervisor's task-local state; N is small (one entry per active
/// session container) so linear scans beat the indirection of a
/// per-slug map.
pub type SessionRegistry = Vec<RunningSession>;

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
/// `Request::ListWorkflows`. `display_name`/`icon` come from
/// `lutin.workflow.display_name` / `lutin.workflow.icon` Docker
/// labels (with fallbacks applied in `workflow_images`). `digest` is
/// the docker image id; the desktop uses it as the cache key for
/// bundle bytes fetched separately via `GetWorkflowBundle`.
pub fn list_workflows() -> Vec<WorkflowInfo> {
    workflow_images::list_installed()
        .into_iter()
        .filter_map(|inst| {
            let id = WorkflowId::parse(&inst.id).ok()?;
            Some(WorkflowInfo {
                id,
                display_name: inst.display_name,
                icon: inst.icon,
                digest: inst.digest,
            })
        })
        .collect()
}

pub async fn start_session(
    registry: &mut SessionRegistry,
    slug: &Slug,
    workflow: &WorkflowId,
    signing: &SigningKey,
    projects_root: &Path,
    global_config_dir: &Path,
) -> Result<(RunningSession, SessionEndpoint), SessionError> {
    let session_id = mint_session_id()?;
    let created_at = chrono::Utc::now().to_rfc3339();
    // Append to the workflow's index *before* the container spawns so
    // a crash mid-spawn still leaves a discoverable (dormant) entry
    // the user can clean up. `spawn_container` is otherwise free to
    // tear down its container; the index entry survives.
    session_index::append(projects_root, slug, workflow, &session_id, &created_at)?;
    spawn_container(
        registry,
        slug,
        workflow,
        &session_id,
        &created_at,
        signing,
        projects_root,
        global_config_dir,
    )
    .await
}

/// Resume a dormant session: discover its workflow id from the index,
/// reuse its existing on-disk state dir, spawn a fresh container.
/// Idempotent — if the session is already running (e.g. a concurrent
/// caller raced us to spawn it), re-mint a token against the existing
/// container instead of erroring. The desktop's open flow funnels
/// every list-row click through OpenSession→ResumeSession fallback,
/// so two near-simultaneous clicks on the same dormant row would
/// otherwise reliably trip this race.
pub async fn resume_session(
    registry: &mut SessionRegistry,
    slug: &Slug,
    session: &SessionId,
    signing: &SigningKey,
    projects_root: &Path,
    global_config_dir: &Path,
) -> Result<(RunningSession, SessionEndpoint), SessionError> {
    if let Some(existing) = registry
        .iter()
        .find(|s| &s.slug == slug && &s.info.id == session)
        .cloned()
    {
        let token = mint_session_token(
            signing,
            slug.clone(),
            existing.info.workflow.clone(),
            existing.info.id.clone(),
        )?;
        let endpoint = SessionEndpoint {
            addr: existing.addr,
            token,
            project_pubkey: existing.project_pubkey.clone(),
        };
        return Ok((existing, endpoint));
    }
    let workflow = session_index::find_workflow(projects_root, slug, session)
        .ok_or_else(|| SessionError::SessionNotFound(session.clone()))?;
    // Recover created_at from the index so the resumed `SessionInfo`
    // keeps its original timestamp. `read_all` is the cheapest
    // workflow-agnostic way to find it.
    let created_at = session_index::read_all(projects_root, slug)
        .into_iter()
        .find(|(_, e)| e.id == *session)
        .map(|(_, e)| e.created_at)
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
    spawn_container(
        registry,
        slug,
        &workflow,
        session,
        &created_at,
        signing,
        projects_root,
        global_config_dir,
    )
    .await
}

/// Spawn a workflow container for `(slug, workflow, session_id)`, wait
/// for handoff, register it. Both `start_session` (fresh id) and
/// `resume_session` (reused id) funnel through here so the docker run
/// flags stay in one place.
async fn spawn_container(
    registry: &mut SessionRegistry,
    slug: &Slug,
    workflow: &WorkflowId,
    session_id: &SessionId,
    created_at: &str,
    signing: &SigningKey,
    projects_root: &Path,
    global_config_dir: &Path,
) -> Result<(RunningSession, SessionEndpoint), SessionError> {
    let image_ref = resolve_workflow_image(workflow)?;
    let project_dir = projects_root.join(slug.as_str());
    let lutin_dir = project_dir.join(".lutin");
    let project_pubkey = ProjectPubkey::new(signing.verifying_key().to_bytes());
    let project_pubkey_b64 = pubkey_to_string(&signing.verifying_key());

    let session_dir = lutin_dir.join("sessions").join(session_id.as_str());
    tokio::fs::create_dir_all(&session_dir).await?;
    let handoff_path = session_dir.join("handoff");
    if let Err(e) = tokio::fs::remove_file(&handoff_path).await
        && e.kind() != std::io::ErrorKind::NotFound
    {
        return Err(SessionError::Io(e));
    }

    let container_name = format!(
        "{CONTAINER_PREFIX}-{slug}-{session}",
        slug = slug.as_str(),
        session = session_id.as_str()
    );

    // Adopt-if-running. Workflows now outlive a CP shutdown (they
    // self-exit on idle), so a "stale" container with our name may
    // actually be a healthy session we just lost track of. Read its
    // handoff and register it instead of killing and respawning.
    // Anything else (exited, partial, name collision after crash)
    // falls through to the rm -f + fresh spawn below.
    if is_container_running(&container_name).await {
        match tokio::fs::read_to_string(&handoff_path).await {
            Ok(s) if !s.trim().is_empty() => {
                if let Ok(addr) = s.trim().parse::<SocketAddr>() {
                    info!(
                        slug = %slug.as_str(),
                        session = %session_id.as_str(),
                        container = %container_name,
                        "adopting running container instead of respawning"
                    );
                    let info = SessionInfo {
                        id: session_id.clone(),
                        workflow: workflow.clone(),
                        created_at: created_at.to_owned(),
                        state: SessionState::Running,
                        summary: None,
                    };
                    let token = mint_session_token(
                        signing,
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
                        slug: slug.clone(),
                        info,
                        addr,
                        container_name,
                        project_pubkey,
                    };
                    registry.push(running.clone());
                    return Ok((running, endpoint));
                }
            }
            _ => {}
        }
    }
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
            "--label",
            &format!("{SESSION_LABEL_SLUG}={}", slug.as_str()),
            "--label",
            &format!("{SESSION_LABEL_WORKFLOW}={}", workflow.as_str()),
            "--label",
            &format!("{SESSION_LABEL_ID}={}", session_id.as_str()),
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
        created_at: created_at.to_owned(),
        state: SessionState::Running,
        // Engine writes summary.json once it has state worth showing;
        // on first start it's None, and `list_sessions` reads it
        // fresh on every call so subsequent writes show up without
        // mutating this snapshot.
        summary: None,
    };
    let token = mint_session_token(
        signing,
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
        slug: slug.clone(),
        info: session_info,
        addr,
        container_name,
        project_pubkey,
    };
    registry.push(running.clone());
    Ok((running, endpoint))
}

pub async fn stop_session(
    registry: &mut SessionRegistry,
    slug: &Slug,
    session: &SessionId,
) -> Result<(), SessionError> {
    let idx = registry
        .iter()
        .position(|s| &s.slug == slug && &s.info.id == session)
        .ok_or_else(|| SessionError::SessionNotFound(session.clone()))?;
    let entry = registry.swap_remove(idx);
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
    signing: &SigningKey,
) -> Result<SessionEndpoint, SessionError> {
    let entry = registry
        .iter()
        .find(|s| &s.slug == slug && &s.info.id == session)
        .ok_or_else(|| SessionError::SessionNotFound(session.clone()))?;
    let token = mint_session_token(
        signing,
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

/// Every session for `slug`, dormant included. Walks the workflow
/// index files on disk for the persistent set, then overrides
/// `state`/`created_at` from the in-memory registry where the same id
/// is currently running. Per-session `summary.json` files are read
/// fresh on every call.
pub fn list_sessions(
    registry: &SessionRegistry,
    projects_root: &Path,
    slug: &Slug,
) -> Vec<SessionInfo> {
    let mut out: Vec<SessionInfo> = session_index::read_all(projects_root, slug)
        .into_iter()
        .map(|(workflow, entry)| SessionInfo {
            id: entry.id.clone(),
            workflow,
            created_at: entry.created_at,
            state: SessionState::Dormant,
            summary: session_summary::read(projects_root, slug, &entry.id),
        })
        .collect();
    for running in registry.iter().filter(|s| &s.slug == slug) {
        if let Some(existing) = out.iter_mut().find(|s| s.id == running.info.id) {
            existing.state = SessionState::Running;
        } else {
            // Running but not in any index file — shouldn't happen
            // (start_session writes the index before spawning) but
            // surface it rather than hide it.
            warn!(
                slug = %slug.as_str(),
                session = %running.info.id,
                "running session has no index entry; surfacing without summary"
            );
            let mut info = running.info.clone();
            info.state = SessionState::Running;
            info.summary = session_summary::read(projects_root, slug, &running.info.id);
            out.push(info);
        }
    }
    out
}

/// Permanently delete a session: stop the container if running, drop
/// the index entry, and remove its on-disk state dir.
pub async fn delete_session(
    registry: &mut SessionRegistry,
    slug: &Slug,
    session: &SessionId,
    projects_root: &Path,
) -> Result<(), SessionError> {
    // Stop if running. Don't error on "not running" — delete should
    // succeed for dormant sessions too.
    if let Some(idx) = registry
        .iter()
        .position(|s| &s.slug == slug && &s.info.id == session)
    {
        let entry = registry.swap_remove(idx);
        let _ = Command::new("docker")
            .args(["rm", "-f", &entry.container_name])
            .output()
            .await;
    }

    let workflow = session_index::find_workflow(projects_root, slug, session)
        .ok_or_else(|| SessionError::SessionNotFound(session.clone()))?;
    session_index::remove(projects_root, slug, &workflow, session)?;

    let session_dir = projects_root
        .join(slug.as_str())
        .join(".lutin")
        .join("sessions")
        .join(session.as_str());
    if let Err(e) = tokio::fs::remove_dir_all(&session_dir).await
        && e.kind() != std::io::ErrorKind::NotFound
    {
        return Err(SessionError::Io(e));
    }
    Ok(())
}

/// Tear down every session container for a given slug — used when the
/// project is deleted, and on supervisor shutdown.
pub async fn stop_all_for_slug(registry: &mut SessionRegistry, slug: &Slug) {
    let mut i = 0;
    while i < registry.len() {
        if &registry[i].slug == slug {
            let s = registry.swap_remove(i);
            let _ = Command::new("docker")
                .args(["rm", "-f", &s.container_name])
                .output()
                .await;
        } else {
            i += 1;
        }
    }
}

/// Drop registry entries whose container is no longer running, and
/// emit `SessionEnded` for each. Called periodically by the
/// supervisor so a workflow that self-exits on idle (or crashes)
/// stops being advertised as Running. Each removed entry is
/// announced via the broadcast so connected desktops update their
/// session list without waiting for the next manual refresh.
pub async fn reap_exited(
    registry: &mut SessionRegistry,
    events: &tokio::sync::broadcast::Sender<lutin_control_protocol::Event>,
) {
    let mut i = 0;
    while i < registry.len() {
        let still_running = is_container_running(&registry[i].container_name).await;
        if still_running {
            i += 1;
            continue;
        }
        let entry = registry.swap_remove(i);
        info!(
            slug = %entry.slug.as_str(),
            session = %entry.info.id,
            container = %entry.container_name,
            "reaping exited session container"
        );
        let _ = events.send(lutin_control_protocol::Event::SessionEnded {
            slug: entry.slug,
            session: entry.info.id,
        });
    }
}

/// Discover running session containers and rebuild a `SessionRegistry`.
/// Called once at supervisor startup so workflows that outlived a CP
/// crash/restart get re-attached instead of orphaned. Each entry needs
/// the project pubkey, which lib.rs supplies via `lookup_pubkey`;
/// containers belonging to projects CP no longer knows about (e.g. the
/// registry file was hand-edited) are skipped with a warning rather
/// than re-imported under a key we can't verify.
pub async fn rehydrate<F>(projects_root: &Path, lookup_pubkey: F) -> SessionRegistry
where
    F: Fn(&Slug) -> Option<ProjectPubkey>,
{
    let discovered = match list_session_containers().await {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "rehydrate: could not list session containers");
            return Vec::new();
        }
    };
    let mut out: SessionRegistry = Vec::new();
    for c in discovered {
        let Some(project_pubkey) = lookup_pubkey(&c.slug) else {
            warn!(
                slug = %c.slug.as_str(),
                container = %c.container_name,
                "rehydrate: container references unknown project; skipping"
            );
            continue;
        };
        let handoff_path = projects_root
            .join(c.slug.as_str())
            .join(".lutin")
            .join("sessions")
            .join(c.session.as_str())
            .join("handoff");
        let addr = match tokio::fs::read_to_string(&handoff_path).await {
            Ok(s) if !s.trim().is_empty() => match s.trim().parse::<SocketAddr>() {
                Ok(a) => a,
                Err(e) => {
                    warn!(
                        slug = %c.slug.as_str(),
                        session = %c.session,
                        error = %e,
                        "rehydrate: invalid handoff addr; skipping"
                    );
                    continue;
                }
            },
            _ => {
                warn!(
                    slug = %c.slug.as_str(),
                    session = %c.session,
                    "rehydrate: container running but handoff missing; skipping"
                );
                continue;
            }
        };
        let created_at = session_index::read_all(projects_root, &c.slug)
            .into_iter()
            .find(|(_, e)| e.id == c.session)
            .map(|(_, e)| e.created_at)
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
        let info = SessionInfo {
            id: c.session.clone(),
            workflow: c.workflow,
            created_at,
            state: SessionState::Running,
            summary: None,
        };
        out.push(RunningSession {
            slug: c.slug,
            info,
            addr,
            container_name: c.container_name,
            project_pubkey,
        });
    }
    out
}

struct DiscoveredContainer {
    container_name: String,
    slug: Slug,
    workflow: WorkflowId,
    session: SessionId,
}

async fn list_session_containers() -> Result<Vec<DiscoveredContainer>, SessionError> {
    let out = Command::new("docker")
        .args([
            "ps",
            "--filter",
            &format!("label={SESSION_LABEL_ID}"),
            "--format",
            &format!(
                "{{{{.Names}}}}\t{{{{.Label \"{SESSION_LABEL_SLUG}\"}}}}\t\
                 {{{{.Label \"{SESSION_LABEL_WORKFLOW}\"}}}}\t\
                 {{{{.Label \"{SESSION_LABEL_ID}\"}}}}"
            ),
        ])
        .output()
        .await?;
    if !out.status.success() {
        return Err(SessionError::Docker {
            op: "ps",
            detail: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        });
    }
    let mut discovered = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() != 4 {
            continue;
        }
        let (Ok(slug), Ok(workflow), Ok(session)) = (
            Slug::parse(parts[1]),
            WorkflowId::parse(parts[2]),
            SessionId::parse(parts[3]),
        ) else {
            warn!(line = %line, "rehydrate: malformed session container labels; skipping");
            continue;
        };
        discovered.push(DiscoveredContainer {
            container_name: parts[0].to_owned(),
            slug,
            workflow,
            session,
        });
    }
    Ok(discovered)
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

/// 128-bit random session id, hex-encoded (32 chars).
fn mint_session_id() -> Result<SessionId, getrandom::Error> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)?;
    Ok(SessionId::from_random_bytes(bytes))
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

