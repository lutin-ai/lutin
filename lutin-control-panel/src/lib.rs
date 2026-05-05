//! Control-panel server. WS endpoint, supervisor-owned project list,
//! request dispatch, broadcast fan-out. Holds the control-panel
//! signing key, mints `Scope::Project(slug)` tokens on `OpenProject`,
//! and spawns the project supervisor subprocess.

pub mod defaults;
mod registry;
pub mod workflow_images;

use futures_util::{SinkExt, StreamExt};
use lutin_auth::{
    Scope, SigningKey, Subject, Ttl, VerifyingKey, mint_with_ttl, pubkey_from_str,
    pubkey_to_string, verify,
};
use lutin_control_protocol::{
    self as cp, ApiError, DisplayName, Event, ProjectEndpoint, ProjectInfo, ProjectPubkey,
    ProjectStatus, Request, Response, ResponseOk, Slug, SpawnFailureKind,
};
use lutin_protocol::{Frame, HandshakeResult, PROTOCOL_VERSION, decode, encode};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, Command as TokioCommand};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};

const CHANNEL_BUF: usize = 64;
/// How long to wait for a freshly-spawned project supervisor to write
/// its handoff file. 5 s is generous; happy-path is well under 100 ms.
const SPAWN_TIMEOUT: Duration = Duration::from_secs(5);
const SPAWN_POLL: Duration = Duration::from_millis(25);

// Per-call budgets for Docker CLI invocations. A wedged Docker daemon
// would otherwise hang the supervisor task indefinitely on any of
// these calls. Generous enough that a healthy daemon under normal
// load never trips them; tight enough that a failure is reported in
// seconds, not forever.
//
// `is_alive`/`image inspect` should answer in milliseconds.
const DOCKER_INSPECT_TIMEOUT: Duration = Duration::from_secs(5);
// `run -d` returns once the container is created (image already
// present, since we preflight). Image fetch from a registry is not
// our path — that's an out-of-band `docker pull`/`docker load`.
const DOCKER_RUN_TIMEOUT: Duration = Duration::from_secs(15);
// `stop --time 50` waits up to 50 s for the container to exit
// gracefully before SIGKILL — workflows inside the container may
// need real time to flush state and detach. The supervisor budget
// adds ~10 s of daemon slack on top.
const DOCKER_STOP_GRACE_SECS: u32 = 50;
const DOCKER_STOP_TIMEOUT: Duration = Duration::from_secs(60);
const DOCKER_RM_TIMEOUT: Duration = Duration::from_secs(10);

/// How often the supervisor sweeps `running` for crashed projects and
/// auto-restarts them. The first tick fires immediately; we drain it
/// after boot auto-launch so it doesn't race the cold-start path.
const HEALTH_INTERVAL: Duration = Duration::from_secs(15);

/// How a project supervisor is launched. `Subprocess` is the default
/// for tests and local dev (no Docker daemon needed). `Docker` is the
/// production path: each project runs in its own container with the
/// per-project tree bind-mounted in.
#[derive(Clone)]
pub enum SpawnBackend {
    Subprocess {
        binary: PathBuf,
    },
    Docker {
        /// Image tag, e.g. `lutin-project:0.1.0`. Built externally and
        /// loaded into the local Docker daemon out-of-band; CP just
        /// references it. Verified present on supervisor boot.
        image: String,
        /// Container name prefix. Each project's container is named
        /// `<container_prefix>-<slug>` — deterministic so boot cleanup
        /// and shutdown can address them by name.
        container_prefix: String,
    },
}

/// Internal spawn error: same shape as `ApiError::SpawnFailed` but
/// not the wire type. Propagates through `ensure_running` →
/// `spawn_project` → `launch_*`/`poll_handoff`/`parse_handoff`, and is
/// converted to the wire variant at the supervisor command boundary.
#[derive(Debug)]
struct SpawnError {
    kind: SpawnFailureKind,
    detail: String,
}

impl SpawnError {
    fn new(kind: SpawnFailureKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

/// Failure mode for parsing a `ProjectLimits` field at the registry
/// boundary. Surfaced as a `serde` deserialization error so a bad
/// value in `projects.toml` fails loudly at boot rather than 30 s
/// later as an opaque `docker run` stderr.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LimitsError {
    #[error("empty {0} value")]
    Empty(&'static str),
    #[error("invalid memory value {0:?}: expected positive integer with optional b/k/m/g suffix")]
    BadMemory(String),
    #[error("invalid cpus value {0:?}: expected positive decimal")]
    BadCpus(String),
}

/// Validated `--memory` string. Accepts a positive integer with an
/// optional case-insensitive unit suffix (`b`/`k`/`m`/`g`), matching
/// Docker's CLI grammar. The original textual form is preserved
/// verbatim to avoid float-precision surprises round-tripping through
/// TOML; the type only certifies it parses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemorySize(String);

impl MemorySize {
    pub fn parse(s: &str) -> Result<Self, LimitsError> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Err(LimitsError::Empty("memory"));
        }
        // Strip optional trailing unit; whatever remains must be a
        // positive integer. We don't normalize unit case — Docker
        // accepts both `2G` and `2g`.
        let last = trimmed.chars().last().expect("non-empty checked above");
        let num_part = match last.to_ascii_lowercase() {
            'b' | 'k' | 'm' | 'g' => &trimmed[..trimmed.len() - last.len_utf8()],
            _ => trimmed,
        };
        let n: u64 = num_part
            .parse()
            .map_err(|_| LimitsError::BadMemory(trimmed.into()))?;
        if n == 0 {
            return Err(LimitsError::BadMemory(trimmed.into()));
        }
        Ok(Self(trimmed.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for MemorySize {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for MemorySize {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// Validated `--cpus` value: a finite positive decimal. Stored as the
/// original string for the same float-precision reason as `MemorySize`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuQuota(String);

impl CpuQuota {
    pub fn parse(s: &str) -> Result<Self, LimitsError> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Err(LimitsError::Empty("cpus"));
        }
        let n: f64 = trimmed
            .parse()
            .map_err(|_| LimitsError::BadCpus(trimmed.into()))?;
        if !n.is_finite() || n <= 0.0 {
            return Err(LimitsError::BadCpus(trimmed.into()));
        }
        Ok(Self(trimmed.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for CpuQuota {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for CpuQuota {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// `--pids-limit` value. `NonZeroU32` rules out a zero-pid container
/// (which would be unable to fork anything, including its own entrypoint).
pub type PidsLimit = std::num::NonZeroU32;

/// Per-project resource caps applied at `docker run`. All fields are
/// optional — a `None` field is forwarded as no flag, i.e. uncapped
/// (Docker default). Subprocess backend ignores these. Stored alongside
/// each project in the registry; operator-edited today, no protocol
/// setter yet. Each field is parsed at the registry boundary so a
/// malformed value fails at load time, not deep inside `docker run`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemorySize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<CpuQuota>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pids: Option<PidsLimit>,
}

/// Server-side project record. The wire-facing [`ProjectInfo`] plus
/// registry-only fields ([`ProjectLimits`]) the supervisor needs at
/// spawn time but doesn't expose to clients today.
#[derive(Debug, Clone)]
pub struct ProjectRecord {
    pub info: ProjectInfo,
    pub limits: ProjectLimits,
}

/// Where to find the project binary and per-project state. Lives in
/// the supervisor task; clients never touch it.
#[derive(Clone)]
pub struct SpawnConfig {
    pub backend: SpawnBackend,
    /// Parent dir of all per-project trees. Each project owns
    /// `<projects_root>/<slug>/`, which contains the user workspace
    /// at the top level and a `.lutin/` subdir with config (settings,
    /// personas, workflows, sessions) plus the CP↔project keypair and
    /// handoff. Single dir per project — bind-mountable as one volume.
    pub projects_root: PathBuf,
    /// Global `.lutin/` directory. Holds workspace-wide settings
    /// (`settings.toml`) plus future global personas/skills/etc. Per
    /// the two-tier resolver, project files override global ones
    /// field-by-field. Bind-mounted read-only into project containers
    /// — CP is the sole writer.
    pub global_config_dir: PathBuf,
}

enum Command {
    ListProjects {
        reply: oneshot::Sender<Response>,
    },
    CreateProject {
        slug: Slug,
        display_name: DisplayName,
        reply: oneshot::Sender<Response>,
    },
    DeleteProject {
        slug: Slug,
        reply: oneshot::Sender<Response>,
    },
    OpenProject {
        slug: Slug,
        reply: oneshot::Sender<Response>,
    },
    StopProject {
        slug: Slug,
        reply: oneshot::Sender<Response>,
    },
}

/// Cheap-clonable handle. Holds the control-panel's pubkey for inbound
/// token verification, an mpsc sender to the supervisor, and the
/// broadcast sender for event fan-out.
#[derive(Clone)]
pub struct AppState {
    pub issuer: VerifyingKey,
    commands: mpsc::Sender<Command>,
    events: broadcast::Sender<Event>,
}

/// Owns the supervisor task. Drop alone leaks; call
/// [`Supervisor::shutdown`] for explicit teardown (also kills any
/// running project subprocesses via tokio's `kill_on_drop`).
pub struct Supervisor {
    pub state: AppState,
    pub join: JoinHandle<()>,
    pub shutdown: oneshot::Sender<()>,
}

impl Supervisor {
    /// Spawn the supervisor task. The control-panel's signing key
    /// doubles as its inbound-token issuer (self-signed bootstrap):
    /// admin tokens minted out-of-band against this key authenticate
    /// to this same server.
    pub fn spawn(signing: SigningKey, config: SpawnConfig) -> Self {
        let issuer = signing.verifying_key();
        let (cmd_tx, cmd_rx) = mpsc::channel(CHANNEL_BUF);
        let (ev_tx, _) = broadcast::channel(CHANNEL_BUF);
        let (sd_tx, sd_rx) = oneshot::channel();
        let join = tokio::spawn(supervisor(cmd_rx, ev_tx.clone(), sd_rx, signing, config));
        let state = AppState {
            issuer,
            commands: cmd_tx,
            events: ev_tx,
        };
        Self {
            state,
            join,
            shutdown: sd_tx,
        }
    }

    pub async fn shutdown(self) {
        // Send error means the supervisor task already finished — fine,
        // nothing to wake up. Join error means it panicked or was
        // cancelled, which we want to surface so a crash isn't silent
        // at process exit.
        let _ = self.shutdown.send(());
        if let Err(e) = self.join.await {
            warn!(error = %e, "supervisor task did not exit cleanly");
        }
    }
}

impl AppState {
    async fn dispatch(&self, req: Request) -> Response {
        let (reply, rx) = oneshot::channel();
        let cmd = match req {
            Request::ListProjects => Command::ListProjects { reply },
            Request::CreateProject { slug, display_name } => Command::CreateProject {
                slug,
                display_name,
                reply,
            },
            Request::DeleteProject { slug } => Command::DeleteProject { slug, reply },
            Request::OpenProject { slug } => Command::OpenProject { slug, reply },
            Request::StopProject { slug } => Command::StopProject { slug, reply },
        };
        if self.commands.send(cmd).await.is_err() {
            return Response::Err(ApiError::Supervisor("supervisor stopped".into()));
        }
        match rx.await {
            Ok(r) => r,
            Err(_) => Response::Err(ApiError::Supervisor("supervisor dropped reply".into())),
        }
    }
}

/// One spawned project supervisor. Lifecycle differs by backend but
/// the two observable bits (where to dial, what pubkey it advertises)
/// are the same — populated identically from the bind-mounted handoff
/// file regardless of backend.
struct RunningEntry {
    addr: SocketAddr,
    project_pubkey: VerifyingKey,
    lifecycle: Lifecycle,
}

enum Lifecycle {
    /// Direct child process. `kill_on_drop(true)` set at spawn, so
    /// dropping the entry on supervisor exit kills the child.
    Subprocess(Child),
    /// Docker container. Lifecycle is owned by the Docker daemon, so
    /// we identify by name and shell out to `docker stop`/`rm` for
    /// teardown — the daemon outlives the CP process by default.
    /// `log_task` follows `docker logs --follow` and forwards lines
    /// into tracing; aborted on terminate so the pump doesn't outlive
    /// its container.
    Docker {
        container_name: String,
        log_task: JoinHandle<()>,
    },
}

/// Result of a liveness probe. `Unknown` distinguishes a wedged
/// daemon / failed `try_wait` / failed exec from a confirmed-dead
/// process; callers can choose to keep an entry around rather than
/// trigger spurious cleanup or auto-restart on transient probe errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Liveness {
    Alive,
    Dead,
    Unknown,
}

impl Lifecycle {
    /// Probe the underlying process/container. Returns `Unknown` for
    /// transient failures (wedged daemon, OS errors on `try_wait`) so
    /// the caller can keep the entry rather than reap it; `Dead` only
    /// when we have positive evidence (process exited, container not
    /// running or no longer exists).
    async fn liveness(&mut self) -> Liveness {
        match self {
            Lifecycle::Subprocess(child) => match child.try_wait() {
                Ok(Some(_)) => Liveness::Dead,
                Ok(None) => Liveness::Alive,
                Err(e) => {
                    warn!(error = %e, "try_wait failed; liveness unknown");
                    Liveness::Unknown
                }
            },
            Lifecycle::Docker { container_name, .. } => {
                // `docker inspect --format {{.State.Running}}` exits 0
                // with stdout "true"/"false" when the container exists,
                // and exits non-zero when it doesn't.
                let mut cmd = TokioCommand::new("docker");
                cmd.args([
                    "inspect",
                    "--format",
                    "{{.State.Running}}",
                    container_name,
                ]);
                match docker_call_warn_on_timeout(
                    &mut cmd,
                    DOCKER_INSPECT_TIMEOUT,
                    "inspect",
                    container_name,
                )
                .await
                {
                    DockerCall::Output(o) if o.status.success() => {
                        if o.stdout.trim_ascii_end() == b"true" {
                            Liveness::Alive
                        } else {
                            Liveness::Dead
                        }
                    }
                    DockerCall::Output(_) => {
                        // Non-zero exit means the container record is
                        // gone (dockerd's `inspect` says "no such
                        // container"); positive evidence of dead.
                        Liveness::Dead
                    }
                    DockerCall::Timeout => Liveness::Unknown,
                    DockerCall::Io(e) => {
                        warn!(
                            error = %e,
                            container = %container_name,
                            "docker inspect exec failed; liveness unknown",
                        );
                        Liveness::Unknown
                    }
                }
            }
        }
    }

    /// Stop the project. Subprocess uses kill+reap; Docker uses
    /// `docker stop --time 5` then `rm -f`. Spawned off the supervisor
    /// task by callers — both can take hundreds of ms.
    async fn terminate(self) {
        match self {
            Lifecycle::Subprocess(mut child) => {
                if let Err(e) = child.kill().await {
                    warn!(error = %e, "failed to send kill to project subprocess");
                }
                if let Err(e) = child.wait().await {
                    warn!(error = %e, "failed to reap project subprocess; possible zombie");
                }
            }
            Lifecycle::Docker { container_name, log_task } => {
                // Stop the log pump first. `docker logs --follow` would
                // exit on its own once the container does, but aborting
                // here drops the JoinHandle deterministically and frees
                // the docker CLI subprocess immediately.
                log_task.abort();
                let grace = DOCKER_STOP_GRACE_SECS.to_string();
                let mut stop = TokioCommand::new("docker");
                stop.args(["stop", "--time", &grace, &container_name]);
                let _ = docker_call_warn_on_timeout(
                    &mut stop,
                    DOCKER_STOP_TIMEOUT,
                    "terminate stop",
                    &container_name,
                )
                .await;
                // `--rm` on `docker run` should make this redundant, but
                // a wedged daemon can leave the container record. Force
                // remove tolerates "no such container".
                let mut rm = TokioCommand::new("docker");
                rm.args(["rm", "-f", &container_name]);
                let _ = docker_call_warn_on_timeout(
                    &mut rm,
                    DOCKER_RM_TIMEOUT,
                    "terminate rm",
                    &container_name,
                )
                .await;
            }
        }
    }
}

async fn supervisor(
    mut rx: mpsc::Receiver<Command>,
    events: broadcast::Sender<Event>,
    mut shutdown: oneshot::Receiver<()>,
    signing: SigningKey,
    config: SpawnConfig,
) {
    let mut projects: Vec<ProjectRecord> = match registry::load(&config.projects_root) {
        Ok(p) => p,
        Err(e) => {
            // Refusing to start avoids silently overwriting a corrupt
            // registry on the first save. Operator must resolve.
            warn!(error = %e, "failed to load project registry; starting empty");
            Vec::new()
        }
    };
    let mut running: Vec<(Slug, RunningEntry)> = Vec::new();

    // Docker-only preflight: image must exist locally (operator's job
    // to `docker load` or `docker pull` it), and any leftover project
    // containers from a prior CP run must be removed before we attempt
    // to start replacements with the same names. On image-missing we
    // log loudly and skip auto-launch — the supervisor stays up so
    // ListProjects keeps working and the operator can fix it.
    let mut image_ok = true;
    if let SpawnBackend::Docker { image, container_prefix } = &config.backend {
        if !docker_image_present(image).await {
            warn!(
                %image,
                "project image not present in local Docker daemon — boot auto-launch disabled. Build/load the image and restart."
            );
            image_ok = false;
        }
        for p in &projects {
            let name = format!("{container_prefix}-{}", p.info.slug.as_str());
            // `rm -f` on a non-existent container is a no-op (exit 1
            // with "no such container" — we ignore exit code).
            let mut cmd = TokioCommand::new("docker");
            cmd.args(["rm", "-f", &name]);
            let _ = docker_call_warn_on_timeout(&mut cmd, DOCKER_RM_TIMEOUT, "boot rm", &name).await;
        }
    }

    // Boot-time auto-launch. Sequential: cold workflow builds are
    // expensive and we'd rather not stampede `cargo` in parallel.
    // Failures are logged + reflected as ProjectStatus::Failed via
    // ensure_running's status fan-out, never aborting the loop.
    if image_ok {
        // Index-based loop: ensure_running takes `&mut projects`, so we
        // can't iterate `projects.iter()` directly. The slug clone per
        // tick is unavoidable (ensure_running needs &Slug), but we
        // skip materializing an intermediate Vec.
        for i in 0..projects.len() {
            let slug = projects[i].info.slug.clone();
            info!(slug = %slug.as_str(), "auto-launching project on boot");
            if let Err(SpawnError { kind, detail }) =
                ensure_running(&slug, &mut projects, &mut running, &signing, &config, &events)
                    .await
            {
                warn!(slug = %slug.as_str(), ?kind, %detail, "boot auto-launch failed");
            }
        }
    }

    let mut health_tick = tokio::time::interval(HEALTH_INTERVAL);
    health_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Drop the immediate-fire first tick: boot auto-launch above just
    // brought everything up, so a sweep right now is wasted work and
    // would also race a still-racy `is_alive` for very-just-spawned
    // entries.
    health_tick.tick().await;

    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => break,
            _ = health_tick.tick() => {
                run_health_check(&mut projects, &mut running, &signing, &config, &events).await;
            }
            cmd = rx.recv() => {
                match cmd {
                    Some(c) => handle_command(c, &mut projects, &mut running, &signing, &config, &events).await,
                    None => break,
                }
            }
        }
    }

    // Drain `running` and tear down each project. Subprocess entries
    // would die on Drop via kill_on_drop, but Docker containers are
    // owned by the daemon and outlive CP unless explicitly stopped —
    // so we always do this on the way out, regardless of backend.
    for (slug, entry) in running.drain(..) {
        info!(slug = %slug.as_str(), "stopping project on shutdown");
        entry.lifecycle.terminate().await;
    }
}

/// Outcome of a timed Docker CLI invocation. `Output` carries the
/// process result for callers that want to inspect stdout/exit; the
/// other two variants distinguish a daemon hang (`Timeout`) from a
/// raw exec/io failure (`Io`) so callers can map them differently.
enum DockerCall {
    Output(std::process::Output),
    Timeout,
    Io(std::io::Error),
}

/// Run a docker invocation with a hard timeout.
async fn docker_call(cmd: &mut TokioCommand, budget: Duration) -> DockerCall {
    match tokio::time::timeout(budget, cmd.output()).await {
        Ok(Ok(out)) => DockerCall::Output(out),
        Ok(Err(e)) => DockerCall::Io(e),
        Err(_) => DockerCall::Timeout,
    }
}

/// Run a docker invocation where the timeout is recoverable: log a
/// warning and continue. Used for fire-and-forget cleanup paths
/// (boot-time `rm`, pre-spawn `rm`, terminate `stop`/`rm`, liveness
/// `inspect`, image-presence `inspect`) where blocking the supervisor
/// is worse than missing the result.
async fn docker_call_warn_on_timeout(
    cmd: &mut TokioCommand,
    budget: Duration,
    op: &str,
    container: &str,
) -> DockerCall {
    let result = docker_call(cmd, budget).await;
    if matches!(result, DockerCall::Timeout) {
        warn!(%op, %container, ?budget, "docker CLI call timed out");
    }
    result
}

async fn docker_image_present(image: &str) -> bool {
    let mut cmd = TokioCommand::new("docker");
    cmd.args(["image", "inspect", image]);
    match docker_call_warn_on_timeout(&mut cmd, DOCKER_INSPECT_TIMEOUT, "image inspect", image).await {
        DockerCall::Output(out) => out.status.success(),
        DockerCall::Timeout | DockerCall::Io(_) => false,
    }
}

async fn handle_command(
    cmd: Command,
    projects: &mut Vec<ProjectRecord>,
    running: &mut Vec<(Slug, RunningEntry)>,
    signing: &SigningKey,
    config: &SpawnConfig,
    events: &broadcast::Sender<Event>,
) {
    match cmd {
        Command::ListProjects { reply } => {
            // `running` is the source of truth for "is this project
            // alive right now". `info.status` stores the transient
            // states (Starting / Failed / Stopped) that aren't derivable
            // from `running` alone. Reconciling on read keeps the two
            // from drifting in client-visible ways.
            let infos: Vec<ProjectInfo> = projects
                .iter()
                .map(|r| {
                    let in_running = running.iter().any(|(s, _)| s == &r.info.slug);
                    let mut info = r.info.clone();
                    info.status = derive_status(&r.info.status, in_running);
                    info
                })
                .collect();
            let _ = reply.send(Response::Ok(ResponseOk::Projects(infos)));
        }
        Command::CreateProject {
            slug,
            display_name,
            reply,
        } => {
            if projects.iter().any(|p| p.info.slug == slug) {
                let _ = reply.send(Response::Err(ApiError::AlreadyExists(slug)));
                return;
            }
            let info = ProjectInfo {
                slug,
                display_name,
                status: ProjectStatus::Stopped,
            };
            // New projects start with no resource caps. Operator can
            // edit `projects.toml` to add a `[project.limits]` table;
            // changes apply on next OpenProject.
            projects.push(ProjectRecord {
                info: info.clone(),
                limits: ProjectLimits::default(),
            });
            if let Err(e) = registry::save(&config.projects_root, projects) {
                warn!(error = %e, "failed to persist project registry after create");
            }
            let _ = reply.send(Response::Ok(ResponseOk::Created(info.clone())));
            let _ = events.send(Event::ProjectCreated(info));
        }
        Command::DeleteProject { slug, reply } => {
            if running.iter().any(|(s, _)| s == &slug) {
                let _ = reply.send(Response::Err(ApiError::ProjectRunning(slug)));
                return;
            }
            let before = projects.len();
            projects.retain(|p| p.info.slug != slug);
            if projects.len() == before {
                let _ = reply.send(Response::Err(ApiError::NotFound(slug)));
                return;
            }
            // Wipe `.lutin/` so a future CreateProject with the same
            // slug starts with a fresh identity instead of silently
            // inheriting the deleted project's keypair. User workspace
            // files at `<projects_root>/<slug>/` are intentionally
            // left in place — destroying the project's identity is
            // not the same as destroying the user's data.
            let lutin_dir = config.projects_root.join(slug.as_str()).join(".lutin");
            if let Err(e) = tokio::fs::remove_dir_all(&lutin_dir).await
                && e.kind() != std::io::ErrorKind::NotFound
            {
                warn!(
                    slug = %slug.as_str(),
                    path = %lutin_dir.display(),
                    error = %e,
                    "failed to wipe .lutin dir on delete; manual cleanup required"
                );
            }
            if let Err(e) = registry::save(&config.projects_root, projects) {
                warn!(error = %e, "failed to persist project registry after delete");
            }
            let _ = reply.send(Response::Ok(ResponseOk::Deleted));
            let _ = events.send(Event::ProjectDeleted { slug });
        }
        Command::OpenProject { slug, reply } => {
            if !projects.iter().any(|p| p.info.slug == slug) {
                let _ = reply.send(Response::Err(ApiError::NotFound(slug)));
                return;
            }
            if let Err(SpawnError { kind, detail }) =
                ensure_running(&slug, projects, running, signing, config, events).await
            {
                let _ = reply.send(Response::Err(ApiError::SpawnFailed { kind, detail }));
                return;
            }
            let entry = running
                .iter()
                .find(|(s, _)| s == &slug)
                .map(|(_, e)| e)
                .expect("ensure_running succeeded");
            let token = match mint_with_ttl(
                signing,
                Subject::parse("control-panel").expect("static subject is valid"),
                Scope::Project(slug.clone()),
                Ttl::from_secs(24 * 60 * 60),
            ) {
                Ok(t) => t,
                Err(e) => {
                    // Mint failure leaves the just-spawned (or
                    // already-running) child intact: it's healthy,
                    // only token issuance is broken. Caller can retry.
                    let _ = reply.send(Response::Err(ApiError::Supervisor(format!("mint: {e}"))));
                    return;
                }
            };
            let endpoint = ProjectEndpoint {
                addr: entry.addr,
                token,
                project_pubkey: ProjectPubkey::new(entry.project_pubkey.to_bytes()),
            };
            let _ = reply.send(Response::Ok(ResponseOk::Opened(endpoint)));
        }
        Command::StopProject { slug, reply } => {
            if !projects.iter().any(|p| p.info.slug == slug) {
                let _ = reply.send(Response::Err(ApiError::NotFound(slug)));
                return;
            }
            if let Some(idx) = running.iter().position(|(s, _)| s == &slug) {
                let (_, entry) = running.swap_remove(idx);
                // Off the supervisor's hot path: terminate() can take
                // hundreds of ms (kill+reap, or `docker stop --time 5`).
                tokio::spawn(async move { entry.lifecycle.terminate().await });
            }
            set_status(projects, &slug, ProjectStatus::Stopped, events);
            let _ = reply.send(Response::Ok(ResponseOk::Stopped));
        }
    }
}

/// Make sure `slug` is in `running`. Reaps a dead entry first, then
/// spawns the project subprocess if needed, fanning out
/// `Starting` / `Running` / `Failed` status events. Shared by the
/// `OpenProject` command and boot-time auto-launch — keep both paths
/// in sync by funneling new spawn cases through here, not duplicating.
async fn ensure_running(
    slug: &Slug,
    projects: &mut [ProjectRecord],
    running: &mut Vec<(Slug, RunningEntry)>,
    signing: &SigningKey,
    config: &SpawnConfig,
    events: &broadcast::Sender<Event>,
) -> Result<(), SpawnError> {
    if let Some(idx) = running.iter().position(|(s, _)| s == slug) {
        // Only reap on confirmed Dead; Unknown (probe failure) keeps
        // the entry so an in-flight request hits the existing endpoint
        // — better than a spurious re-spawn on a transient daemon hiccup.
        if running[idx].1.lifecycle.liveness().await == Liveness::Dead {
            running.swap_remove(idx);
            set_status(projects, slug, ProjectStatus::Failed, events);
        }
    }
    if running.iter().any(|(s, _)| s == slug) {
        return Ok(());
    }
    // Snapshot limits before the (mutable) status update — keeps a
    // single immutable borrow of the record's limits across the await
    // without clashing with set_status's mutable reborrow.
    let limits = projects
        .iter()
        .find(|p| &p.info.slug == slug)
        .map(|p| p.limits.clone())
        .unwrap_or_default();
    set_status(projects, slug, ProjectStatus::Starting, events);
    match spawn_project(slug, config, &signing.verifying_key(), &limits).await {
        Ok(entry) => {
            running.push((slug.clone(), entry));
            set_status(projects, slug, ProjectStatus::Running, events);
            Ok(())
        }
        Err(e) => {
            set_status(projects, slug, ProjectStatus::Failed, events);
            Err(e)
        }
    }
}

/// Sweep `running` for entries whose underlying process/container has
/// died and attempt to restart each one. Only operates on entries that
/// were *meant* to be running (i.e. currently in `running`); a project
/// the operator deliberately stopped is no longer in that list and is
/// left alone.
///
/// Failure to restart re-marks the project Failed and logs; the next
/// tick will retry. Repeated failures stay logged as warnings rather
/// than escalating — there's no separate alerting channel to escalate
/// into yet.
async fn run_health_check(
    projects: &mut [ProjectRecord],
    running: &mut Vec<(Slug, RunningEntry)>,
    signing: &SigningKey,
    config: &SpawnConfig,
    events: &broadcast::Sender<Event>,
) {
    let dead = prune_dead(running, projects, events).await;
    for slug in dead {
        info!(slug = %slug.as_str(), "project died; attempting auto-restart");
        if let Err(SpawnError { kind, detail }) =
            ensure_running(&slug, projects, running, signing, config, events).await
        {
            warn!(
                slug = %slug.as_str(),
                ?kind,
                %detail,
                "auto-restart failed; will retry on next health tick",
            );
        }
    }
}

/// Drop dead entries from `running`, marking each one's project
/// `Failed`, and return their slugs so the caller can choose to
/// restart them. Split out from `run_health_check` so the
/// death-detection arm is unit-testable without also wiring up a
/// spawnable project binary for the restart arm.
async fn prune_dead(
    running: &mut Vec<(Slug, RunningEntry)>,
    projects: &mut [ProjectRecord],
    events: &broadcast::Sender<Event>,
) -> Vec<Slug> {
    let mut dead = Vec::new();
    let mut idx = 0;
    while idx < running.len() {
        // Only confirmed-Dead entries are pruned. `Unknown` (e.g.
        // wedged daemon, OS error on `try_wait`) leaves the entry in
        // place so we don't tear down a project on a transient probe
        // failure; the next tick re-probes.
        match running[idx].1.lifecycle.liveness().await {
            Liveness::Dead => {
                let (slug, _) = running.swap_remove(idx);
                set_status(projects, &slug, ProjectStatus::Failed, events);
                dead.push(slug);
            }
            Liveness::Alive | Liveness::Unknown => idx += 1,
        }
    }
    dead
}

/// Derive the externally-visible status from the stored value plus
/// whether the project currently appears in `running`. `running`
/// membership is authoritative for "live" / "not live"; the stored
/// value is consulted only for the states `running` cannot represent
/// (Starting transient, last spawn outcome).
///
/// Cases:
/// - In `running` → always `Running`, regardless of stale stored
///   value (covers the window between spawn success and the
///   `set_status(Running)` call, and any drift).
/// - Not in `running` but stored says `Running` → must have died
///   between status updates; surface as `Failed` rather than the
///   stale Running.
/// - Not in `running` and stored is anything else → the stored
///   transient is the truth (Stopped, Starting in flight, Failed).
fn derive_status(stored: &ProjectStatus, in_running: bool) -> ProjectStatus {
    if in_running {
        return ProjectStatus::Running;
    }
    match stored {
        ProjectStatus::Running => ProjectStatus::Failed,
        other => other.clone(),
    }
}

fn set_status(
    projects: &mut [ProjectRecord],
    slug: &Slug,
    status: ProjectStatus,
    events: &broadcast::Sender<Event>,
) {
    if let Some(p) = projects.iter_mut().find(|p| &p.info.slug == slug)
        && p.info.status != status
    {
        p.info.status = status.clone();
        let _ = events.send(Event::ProjectStatusChanged {
            slug: slug.clone(),
            status,
        });
    }
}


async fn spawn_project(
    slug: &Slug,
    config: &SpawnConfig,
    issuer: &VerifyingKey,
    limits: &ProjectLimits,
) -> Result<RunningEntry, SpawnError> {
    // Per-project tree at `<projects_root>/<slug>/`. The user's
    // workspace files live at the top level; `.lutin/` holds config
    // (settings, personas, workflows, sessions) plus the CP↔project
    // keypair and handoff. Pre-create the `.lutin/` subdir so a
    // future Docker bind mount has a real source dir to attach to
    // (Docker would otherwise auto-create as root and lock us out).
    let project_dir = config.projects_root.join(slug.as_str());
    let lutin_dir = project_dir.join(".lutin");
    tokio::fs::create_dir_all(&lutin_dir).await.map_err(|e| {
        SpawnError::new(
            SpawnFailureKind::Io,
            format!("create project dir {}: {e}", lutin_dir.display()),
        )
    })?;
    let keypair_path = lutin_dir.join("keypair");
    let handoff_path = lutin_dir.join("handoff");
    // Stale handoff from a prior run would let us read the OLD
    // pubkey+addr and connect to a dead port. Remove before spawn,
    // tolerating only NotFound; any other error (e.g. EACCES) is a
    // genuine condition we must not silently swallow.
    match tokio::fs::remove_file(&handoff_path).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(SpawnError::new(
                SpawnFailureKind::Io,
                format!("remove stale handoff {}: {e}", handoff_path.display()),
            ));
        }
    }

    let mut lifecycle = match &config.backend {
        SpawnBackend::Subprocess { binary } => {
            launch_subprocess(binary, slug, issuer, &lutin_dir, &handoff_path, &keypair_path, &config.global_config_dir)?
        }
        SpawnBackend::Docker { image, container_prefix } => {
            launch_docker(image, container_prefix, slug, issuer, &project_dir, &config.global_config_dir, limits).await?
        }
    };

    // Poll the bind-mounted/host handoff file. Liveness check on each
    // tick catches a backend that died before publishing — without it
    // we'd just hit the deadline and report a misleading timeout.
    let handoff_text = match poll_handoff(&handoff_path, &mut lifecycle).await {
        Ok(s) => s,
        Err(e) => {
            lifecycle.terminate().await;
            return Err(e);
        }
    };
    match parse_handoff(&handoff_text) {
        Ok((addr, project_pubkey)) => Ok(RunningEntry {
            addr,
            project_pubkey,
            lifecycle,
        }),
        Err(e) => {
            lifecycle.terminate().await;
            Err(e)
        }
    }
}

fn launch_subprocess(
    binary: &Path,
    slug: &Slug,
    issuer: &VerifyingKey,
    lutin_dir: &Path,
    handoff_path: &Path,
    keypair_path: &Path,
    global_config_dir: &Path,
) -> Result<Lifecycle, SpawnError> {
    let mut cmd = TokioCommand::new(binary);
    cmd.env("LUTIN_PROJECT_SLUG", slug.as_str())
        .env("LUTIN_PROJECT_ISSUER_PUBKEY", pubkey_to_string(issuer))
        .env("LUTIN_PROJECT_KEYPAIR_PATH", keypair_path)
        .env("LUTIN_PROJECT_HANDOFF_PATH", handoff_path)
        .env("LUTIN_PROJECT_ADDR", "127.0.0.1:0")
        .env("LUTIN_GLOBAL_CONFIG_DIR", global_config_dir)
        .env("LUTIN_PROJECT_CONFIG_DIR", lutin_dir)
        .kill_on_drop(true);
    // No explicit LUTIN_PROJECT_WORKFLOWS_DIR: lutin-project falls
    // back to `<config_dir>/workflows`, i.e. inside `.lutin/`. Keeps
    // workflow source + cargo `target/` inside the single per-project
    // tree (one bind mount captures everything in Docker mode).
    let child = cmd.spawn().map_err(|e| {
        let kind = if e.kind() == std::io::ErrorKind::NotFound {
            SpawnFailureKind::BinaryMissing
        } else {
            SpawnFailureKind::Io
        };
        SpawnError::new(kind, format!("spawn {}: {e}", binary.display()))
    })?;
    Ok(Lifecycle::Subprocess(child))
}

async fn launch_docker(
    image: &str,
    container_prefix: &str,
    slug: &Slug,
    issuer: &VerifyingKey,
    project_dir: &Path,
    global_config_dir: &Path,
    limits: &ProjectLimits,
) -> Result<Lifecycle, SpawnError> {
    let container_name = format!("{container_prefix}-{}", slug.as_str());
    // Defensive: a stale container with this name (CP crash, prior boot
    // cleanup skipped) would make `docker run --name` fail with name
    // conflict. `rm -f` is idempotent — exits 0 with "no such" on stderr
    // when the name is free, so we don't gate on its exit code. Timeout
    // here doesn't fail the spawn outright; if there's a real conflict
    // the subsequent `docker run` will surface it.
    let mut pre_rm = TokioCommand::new("docker");
    pre_rm.args(["rm", "-f", &container_name]);
    let _ = docker_call_warn_on_timeout(
        &mut pre_rm,
        DOCKER_RM_TIMEOUT,
        "pre-spawn rm",
        &container_name,
    )
    .await;

    // SAFETY: getuid/getgid are signal-safe and never fail.
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };

    // Container-side paths are fixed regardless of host layout. The
    // env vars below tell lutin-project where to look INSIDE the
    // container; the bind mounts make those paths point at the host
    // tree at `<projects_root>/<slug>/` and the global config dir.
    let project_mount = format!("{}:/project", project_dir.display());
    let global_mount = format!("{}:/global:ro", global_config_dir.display());
    let user_arg = format!("{uid}:{gid}");

    let mut cmd = TokioCommand::new("docker");
    cmd.args([
        "run",
        "-d",
        "--rm",
        "--name",
        &container_name,
        // Host network: the container binds 127.0.0.1:0 inside, kernel
        // picks a port, project writes the addr to the bind-mounted
        // handoff. CP reads it from the host loopback. Identical to
        // the subprocess flow — no protocol change.
        "--network=host",
        "--user",
        &user_arg,
        "-v",
        &project_mount,
        "-v",
        &global_mount,
    ]);
    // Resource caps. Each flag is only emitted when the operator set a
    // value — `None` means "no flag", which Docker treats as uncapped.
    if let Some(memory) = &limits.memory {
        cmd.args(["--memory", memory.as_str()]);
    }
    if let Some(cpus) = &limits.cpus {
        cmd.args(["--cpus", cpus.as_str()]);
    }
    if let Some(pids) = limits.pids {
        cmd.arg("--pids-limit").arg(pids.get().to_string());
    }
    cmd.args([
        "-e",
        &format!("LUTIN_PROJECT_SLUG={}", slug.as_str()),
        "-e",
        &format!("LUTIN_PROJECT_ISSUER_PUBKEY={}", pubkey_to_string(issuer)),
        "-e",
        "LUTIN_PROJECT_KEYPAIR_PATH=/project/.lutin/keypair",
        "-e",
        "LUTIN_PROJECT_HANDOFF_PATH=/project/.lutin/handoff",
        "-e",
        "LUTIN_PROJECT_ADDR=127.0.0.1:0",
        "-e",
        "LUTIN_PROJECT_CONFIG_DIR=/project/.lutin",
        "-e",
        "LUTIN_GLOBAL_CONFIG_DIR=/global",
        image,
    ]);
    let out = match docker_call(&mut cmd, DOCKER_RUN_TIMEOUT).await {
        DockerCall::Output(out) => out,
        DockerCall::Io(e) => {
            let kind = if e.kind() == std::io::ErrorKind::NotFound {
                // `docker` CLI itself missing — operator hasn't installed it.
                SpawnFailureKind::BinaryMissing
            } else {
                SpawnFailureKind::Io
            };
            return Err(SpawnError::new(kind, format!("invoke docker run: {e}")));
        }
        DockerCall::Timeout => {
            return Err(SpawnError::new(
                SpawnFailureKind::DaemonUnresponsive,
                format!(
                    "docker run did not return within {DOCKER_RUN_TIMEOUT:?}; daemon may be wedged",
                ),
            ));
        }
    };
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(SpawnError::new(
            SpawnFailureKind::Io,
            format!("docker run exited {}: {}", out.status, stderr.trim()),
        ));
    }
    let log_task = spawn_log_pump(slug.clone(), container_name.clone());
    Ok(Lifecycle::Docker {
        container_name,
        log_task,
    })
}

/// Follow a container's stdout/stderr and forward each line into
/// `tracing`, tagged with the project slug. Returns immediately; the
/// pump runs until the container exits (at which point `docker logs`
/// returns) or until the JoinHandle is aborted.
///
/// `--tail=0` skips backlog (we only care about post-launch output).
/// `--timestamps` would prepend an RFC3339 stamp, but tracing already
/// stamps each event, so we don't bother. Each docker stream maps to a
/// fixed tracing level: stdout → info, stderr → warn. lutin-project's
/// own tracing setup writes its events to stderr, so warn-by-default
/// surfaces real problems without us having to parse log levels.
fn spawn_log_pump(slug: Slug, container_name: String) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut child = match TokioCommand::new("docker")
            .args(["logs", "--follow", "--tail=0", &container_name])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    slug = %slug.as_str(),
                    container = %container_name,
                    error = %e,
                    "failed to spawn docker logs follower; container output will not be captured"
                );
                return;
            }
        };
        // Each stream gets its own task so a slow consumer on one
        // doesn't backpressure the other. Both finish naturally when
        // `docker logs` closes the pipe at container exit.
        let stdout_task = child.stdout.take().map(|s| {
            let slug = slug.clone();
            let cn = container_name.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(s).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    info!(slug = %slug.as_str(), container = %cn, stream = "stdout", "{line}");
                }
            })
        });
        let stderr_task = child.stderr.take().map(|s| {
            let slug = slug.clone();
            let cn = container_name.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(s).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    warn!(slug = %slug.as_str(), container = %cn, stream = "stderr", "{line}");
                }
            })
        });
        // `docker logs --follow` exits when the container does, or
        // when its stdout is closed — either way `wait` should yield
        // a successful exit. An error here means we couldn't reap it
        // (rare: signal interruption on the child reap).
        if let Err(e) = child.wait().await {
            warn!(slug = %slug.as_str(), error = %e, "failed to reap docker logs follower");
        }
        // Surface panics in the per-stream readers — otherwise a
        // poisoned line decoder would silently stop forwarding logs
        // and the operator would never know the pump went deaf.
        if let Some(t) = stdout_task
            && let Err(e) = t.await
        {
            warn!(slug = %slug.as_str(), error = %e, "stdout log reader task aborted");
        }
        if let Some(t) = stderr_task
            && let Err(e) = t.await
        {
            warn!(slug = %slug.as_str(), error = %e, "stderr log reader task aborted");
        }
    })
}

async fn poll_handoff(
    handoff_path: &Path,
    lifecycle: &mut Lifecycle,
) -> Result<String, SpawnError> {
    let deadline = Instant::now() + SPAWN_TIMEOUT;
    loop {
        // Only confirmed-Dead aborts the wait. Unknown keeps polling —
        // the SPAWN_TIMEOUT deadline catches a genuinely-stuck child
        // anyway, and a transient probe error shouldn't masquerade as
        // ChildExited.
        if lifecycle.liveness().await == Liveness::Dead {
            return Err(SpawnError::new(
                SpawnFailureKind::ChildExited,
                "project supervisor exited before publishing handoff",
            ));
        }
        match tokio::fs::read_to_string(handoff_path).await {
            Ok(s) => return Ok(s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(SpawnError::new(
                    SpawnFailureKind::Io,
                    format!("read handoff {}: {e}", handoff_path.display()),
                ));
            }
        }
        if Instant::now() > deadline {
            return Err(SpawnError::new(
                SpawnFailureKind::HandoffTimeout,
                format!("project supervisor did not publish handoff file within {SPAWN_TIMEOUT:?}"),
            ));
        }
        tokio::time::sleep(SPAWN_POLL).await;
    }
}

fn parse_handoff(text: &str) -> Result<(SocketAddr, VerifyingKey), SpawnError> {
    // write_atomic guarantees readers see all-or-nothing, so a present
    // file is always complete. We removed any prior file pre-spawn, so
    // any parse failure here indicates a malformed handoff from THIS
    // child — propagate.
    let mut lines = text.lines();
    let pubkey_line = lines.next().ok_or_else(|| {
        SpawnError::new(SpawnFailureKind::InvalidHandoff, "handoff missing pubkey line")
    })?;
    let addr_line = lines.next().ok_or_else(|| {
        SpawnError::new(SpawnFailureKind::InvalidHandoff, "handoff missing addr line")
    })?;
    let project_pubkey = pubkey_from_str(pubkey_line.trim()).map_err(|e| {
        SpawnError::new(
            SpawnFailureKind::InvalidHandoff,
            format!("parse project pubkey: {e}"),
        )
    })?;
    let addr: SocketAddr = addr_line.trim().parse().map_err(|e| {
        SpawnError::new(
            SpawnFailureKind::InvalidHandoff,
            format!("parse project addr {addr_line:?}: {e}"),
        )
    })?;
    Ok((addr, project_pubkey))
}

pub async fn run(listener: TcpListener, state: AppState) -> anyhow::Result<()> {
    loop {
        let (sock, peer) = listener.accept().await?;
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = serve_conn(sock, state).await {
                warn!(%peer, error = %e, "connection ended");
            }
        });
    }
}

async fn serve_conn(sock: TcpStream, state: AppState) -> anyhow::Result<()> {
    let ws = tokio_tungstenite::accept_async(sock).await?;
    let (mut tx, mut rx) = ws.split();

    let Some(msg) = rx.next().await else {
        return Ok(());
    };
    let bytes = match msg? {
        Message::Binary(b) => b,
        _ => anyhow::bail!("expected binary hello"),
    };
    let frame = decode(&bytes)?;
    let Frame::Hello {
        protocol_version,
        token,
    } = frame
    else {
        anyhow::bail!("expected Hello");
    };
    if protocol_version != PROTOCOL_VERSION {
        let nack = encode(&Frame::HelloAck(HandshakeResult::Rejected {
            reason: format!(
                "protocol version mismatch: server={PROTOCOL_VERSION} client={protocol_version}"
            ),
        }))?;
        tx.send(Message::Binary(nack.into())).await?;
        return Ok(());
    }
    match verify(&token, &state.issuer) {
        Ok(claims) if matches!(claims.scope, Scope::ControlPanel) => {}
        Ok(_) => {
            let nack = encode(&Frame::HelloAck(HandshakeResult::Rejected {
                reason: "scope must be ControlPanel".into(),
            }))?;
            tx.send(Message::Binary(nack.into())).await?;
            return Ok(());
        }
        Err(e) => {
            let nack = encode(&Frame::HelloAck(HandshakeResult::Rejected {
                reason: format!("auth: {e}"),
            }))?;
            tx.send(Message::Binary(nack.into())).await?;
            return Ok(());
        }
    }
    let ack = encode(&Frame::HelloAck(HandshakeResult::Accepted))?;
    tx.send(Message::Binary(ack.into())).await?;

    let mut events = state.events.subscribe();

    loop {
        tokio::select! {
            biased;

            ev = events.recv() => match ev {
                Ok(e) => {
                    let body = cp::encode(&e)?;
                    let frame = encode(&Frame::Broadcast { body })?;
                    if tx.send(Message::Binary(frame.into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(n, "client lagged events; dropping connection to force resync");
                    break;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },

            msg = rx.next() => {
                let Some(msg) = msg else { break };
                let bytes = match msg? {
                    Message::Binary(b) => b,
                    Message::Close(_) => break,
                    Message::Ping(p) => {
                        tx.send(Message::Pong(p)).await?;
                        continue;
                    }
                    _ => continue,
                };
                let frame = decode(&bytes)?;
                match frame {
                    Frame::Payload { request_id, body } => {
                        let req = cp::decode::<Request>(&body)?;
                        let resp = state.dispatch(req).await;
                        let body = cp::encode(&resp)?;
                        let out = encode(&Frame::Payload { request_id, body })?;
                        tx.send(Message::Binary(out.into())).await?;
                    }
                    Frame::Ping { nonce } => {
                        let out = encode(&Frame::Pong { nonce })?;
                        tx.send(Message::Binary(out.into())).await?;
                    }
                    Frame::Close { .. } => break,
                    frame => {
                        warn!(?frame, "unexpected frame from client");
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lutin_control_protocol::DisplayName;
    use lutin_keypair::load_or_create_keypair;
    use tempfile::TempDir;

    /// Minimal RunningEntry around a Lifecycle. Fields beyond
    /// `lifecycle` are unobserved by `prune_dead`/`run_health_check`,
    /// but we still need real values: VerifyingKey rejects arbitrary
    /// bytes, so we mint a throwaway keypair into a temp file.
    fn fake_entry(lifecycle: Lifecycle, kp_dir: &std::path::Path) -> RunningEntry {
        let signing = load_or_create_keypair(&kp_dir.join("kp")).unwrap();
        RunningEntry {
            addr: "127.0.0.1:0".parse().unwrap(),
            project_pubkey: signing.verifying_key(),
            lifecycle,
        }
    }

    fn fake_record(slug: &str) -> ProjectRecord {
        ProjectRecord {
            info: ProjectInfo {
                slug: Slug::parse(slug).unwrap(),
                display_name: DisplayName::parse(slug).unwrap(),
                status: ProjectStatus::Running,
            },
            limits: ProjectLimits::default(),
        }
    }

    /// `docker_call` returns Output for a process that exits within
    /// budget. Uses `true` (POSIX) — always present, exits 0 instantly,
    /// no flag-parsing variance across platforms.
    #[tokio::test]
    async fn docker_call_returns_output_for_quick_command() {
        let mut cmd = TokioCommand::new("true");
        match docker_call(&mut cmd, Duration::from_secs(5)).await {
            DockerCall::Output(out) => assert!(out.status.success()),
            DockerCall::Timeout => panic!("`true` shouldn't time out at 5 s"),
            DockerCall::Io(e) => panic!("unexpected io error: {e}"),
        }
    }

    /// A long-running command past the budget collapses to `Timeout`
    /// rather than blocking the awaiting task. `sleep 5` with a 50 ms
    /// budget is the smallest unambiguous variant.
    #[tokio::test]
    async fn docker_call_times_out_when_exceeding_budget() {
        let mut cmd = TokioCommand::new("sleep");
        cmd.arg("5");
        let res = docker_call(&mut cmd, Duration::from_millis(50)).await;
        assert!(
            matches!(res, DockerCall::Timeout),
            "expected Timeout, got something else"
        );
    }

    /// Exec failure (binary missing) surfaces as `Io`, distinct from
    /// `Timeout` so callers can map to BinaryMissing vs DaemonUnresponsive.
    #[tokio::test]
    async fn docker_call_returns_io_for_missing_binary() {
        let mut cmd = TokioCommand::new("/no/such/__lutin_test_binary_x9q2");
        let res = docker_call(&mut cmd, Duration::from_secs(5)).await;
        match res {
            DockerCall::Io(e) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound),
            other => panic!(
                "expected Io(NotFound), got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    /// Dead subprocess entries should be removed from `running`, the
    /// project status flipped to `Failed`, and a `ProjectStatusChanged`
    /// event broadcast. `true` is the smallest binary that exits 0
    /// instantly — once `wait()` returns, `is_alive()` will report
    /// false on the next probe.
    #[tokio::test]
    async fn prune_dead_removes_exited_subprocess_and_marks_failed() {
        let kp_dir = TempDir::new().unwrap();
        let mut child = TokioCommand::new("true").kill_on_drop(true).spawn().unwrap();
        let _ = child.wait().await;
        let slug = Slug::parse("dead").unwrap();

        let mut running = vec![(
            slug.clone(),
            fake_entry(Lifecycle::Subprocess(child), kp_dir.path()),
        )];
        let mut projects = vec![fake_record("dead")];
        let (events, mut rx) = broadcast::channel(8);

        let dead = prune_dead(&mut running, &mut projects, &events).await;

        assert_eq!(dead, vec![slug.clone()]);
        assert!(
            running.is_empty(),
            "dead entry should have been swap_removed"
        );
        assert_eq!(projects[0].info.status, ProjectStatus::Failed);
        match rx.try_recv() {
            Ok(Event::ProjectStatusChanged { slug: s, status }) => {
                assert_eq!(s, slug);
                assert_eq!(status, ProjectStatus::Failed);
            }
            other => panic!("expected ProjectStatusChanged(Failed), got {other:?}"),
        }
    }

    /// Membership in `running` is the source of truth for "live".
    /// Stored Running while not in running means the entry died
    /// before set_status caught up — clients see Failed, not stale Running.
    #[test]
    fn derive_status_reconciles_drift() {
        // In running → always Running, even if stored is something else.
        assert_eq!(
            derive_status(&ProjectStatus::Stopped, true),
            ProjectStatus::Running,
        );
        assert_eq!(
            derive_status(&ProjectStatus::Failed, true),
            ProjectStatus::Running,
        );
        // Not in running, stored Running → Failed (drift safety net).
        assert_eq!(
            derive_status(&ProjectStatus::Running, false),
            ProjectStatus::Failed,
        );
        // Not in running, stored anything else → stored value passes
        // through (Stopped, Starting in-flight, Failed).
        assert_eq!(
            derive_status(&ProjectStatus::Stopped, false),
            ProjectStatus::Stopped,
        );
        assert_eq!(
            derive_status(&ProjectStatus::Starting, false),
            ProjectStatus::Starting,
        );
        assert_eq!(
            derive_status(&ProjectStatus::Failed, false),
            ProjectStatus::Failed,
        );
    }

    /// Live subprocess entries are left alone — `prune_dead` must not
    /// drop a healthy project just because a tick fired.
    #[tokio::test]
    async fn prune_dead_keeps_live_subprocess_untouched() {
        let kp_dir = TempDir::new().unwrap();
        let child = TokioCommand::new("sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let slug = Slug::parse("alive").unwrap();

        let mut running = vec![(
            slug.clone(),
            fake_entry(Lifecycle::Subprocess(child), kp_dir.path()),
        )];
        let mut projects = vec![fake_record("alive")];
        // Caller would normally have set status=Running before the tick.
        projects[0].info.status = ProjectStatus::Running;
        let (events, mut rx) = broadcast::channel(8);

        let dead = prune_dead(&mut running, &mut projects, &events).await;

        assert!(dead.is_empty());
        assert_eq!(running.len(), 1);
        assert_eq!(projects[0].info.status, ProjectStatus::Running);
        // No status transition → no event emitted.
        assert!(rx.try_recv().is_err());

        // Tear down the sleep child so it doesn't outlive the test.
        let (_, entry) = running.pop().unwrap();
        entry.lifecycle.terminate().await;
    }
}
