//! Chat workflow engine binary.
//!
//! One subprocess per session. Spawned by `lutin-project` with the env
//! handoff documented under `Env`. Binds a loopback TCP listener,
//! publishes its bound addr to `LUTIN_WORKFLOW_HANDOFF_PATH` so the
//! supervisor can hand it to clients, then serves WebSocket
//! connections protected by `WorkflowSession`-scoped tokens issued by
//! the project tier.
//!
//! The binary itself is now a thin bootstrap: it parses env, sets up
//! channels, spawns the runner and registry actors, and serves
//! connections. The per-turn machinery lives in sibling modules
//! (`runner`, `turn`, `rewind`, `compaction`, `mutation`, etc.).

mod agent_build;
mod agents;
mod compaction;
mod mutation;
mod principle;
mod projection;
mod review;
mod reviewer;
mod rewind;
mod runner;
mod step;
mod store;
mod subagents_glue;
mod tools;
mod turn;
mod wire;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use anyhow::{Context, Result, anyhow};
use lutin_auth::{SessionId, Slug, VerifyingKey, WorkflowId, pubkey_from_str};
use lutin_storage::Resolver;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc, watch};
use tracing::{info, warn};

use crate::runner::{RunnerCtx, run_agent_loop};
use crate::subagents_glue::run_subagent_task;
use crate::wire::{AppState, serve_conn};

/// Env vars the supervisor sets before exec. All are required; missing
/// or malformed values are a hard error — we'd rather fail fast at
/// startup than serve a half-configured session.
struct Env {
    project: Slug,
    project_pubkey: VerifyingKey,
    workflow: WorkflowId,
    session: SessionId,
    state_dir: PathBuf,
    addr: SocketAddr,
    handoff_path: PathBuf,
    global_config_dir: PathBuf,
    project_config_dir: PathBuf,
}

impl Env {
    fn from_process() -> Result<Self> {
        Ok(Self {
            project: Slug::parse(require_env("LUTIN_PROJECT_SLUG")?)
                .map_err(|e| anyhow!("LUTIN_PROJECT_SLUG: {e}"))?,
            project_pubkey: pubkey_from_str(&require_env("LUTIN_PROJECT_PUBKEY")?)
                .map_err(|e| anyhow!("LUTIN_PROJECT_PUBKEY: {e}"))?,
            workflow: WorkflowId::parse(require_env("LUTIN_WORKFLOW_ID")?)
                .map_err(|e| anyhow!("LUTIN_WORKFLOW_ID: {e}"))?,
            session: SessionId::parse(require_env("LUTIN_SESSION_ID")?)
                .map_err(|e| anyhow!("LUTIN_SESSION_ID: {e}"))?,
            state_dir: PathBuf::from(require_env("LUTIN_SESSION_STATE_DIR")?),
            addr: require_env("LUTIN_WORKFLOW_ADDR")?
                .parse()
                .context("LUTIN_WORKFLOW_ADDR is not a valid socket addr")?,
            handoff_path: PathBuf::from(require_env("LUTIN_WORKFLOW_HANDOFF_PATH")?),
            global_config_dir: PathBuf::from(require_env("LUTIN_GLOBAL_CONFIG_DIR")?),
            project_config_dir: PathBuf::from(require_env("LUTIN_PROJECT_CONFIG_DIR")?),
        })
    }
}

fn require_env(key: &'static str) -> Result<String> {
    std::env::var(key).map_err(|_| anyhow!("missing required env var: {key}"))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let env = Env::from_process()?;

    let listener = TcpListener::bind(env.addr)
        .await
        .with_context(|| format!("bind {}", env.addr))?;
    let bound = listener.local_addr()?;
    info!(%bound, session = %env.session, "chat workflow listening");

    lutin_keypair::write_atomic(&env.handoff_path, format!("{bound}\n").as_bytes(), 0o600)
        .with_context(|| format!("write handoff {}", env.handoff_path.display()))?;

    // Capacity sized for the principled workflow's busy event stream.
    // Each gated tool call emits ~2 events per active principle
    // (ReviewerStarted + ReviewerCompleted) plus frame open/progress/
    // resolved plus delta/reasoning chunks. With ~30 principles in the
    // default order, a single step can easily exceed 64 events between
    // chrome polls; 4096 keeps a comfortable margin even when several
    // steps queue up back-to-back.
    let (events, _) = broadcast::channel(4096);
    let (agent_cmds, agent_rx) = mpsc::unbounded_channel();
    let (runner_failure_tx, runner_failure_rx) = watch::channel::<Option<String>>(None);
    let next_turn = Arc::new(AtomicU64::new(1));
    let resolver = Arc::new(Resolver::new(
        env.global_config_dir.clone(),
        Some(env.project_config_dir.clone()),
    ));

    // Pre-create the registry's command + completion channels so we
    // can hand `agent_registry` to `RunnerCtx` (which the spawner
    // captures) before the actor is launched.
    let (agent_registry_tx, agent_registry_rx) = mpsc::unbounded_channel();
    let (completions_tx, completions_rx) = mpsc::unbounded_channel();

    let runner_ctx = RunnerCtx {
        state_dir: env.state_dir.clone(),
        project_config_dir: env.project_config_dir.clone(),
        resolver: resolver.clone(),
        events: events.clone(),
        failure: runner_failure_tx,
        next_turn: next_turn.clone(),
        agent_registry: agent_registry_tx.clone(),
    };
    let spawner_ctx = runner_ctx.clone();
    let spawner: agents::Spawner = Box::new(move |id, spec, update_tx| {
        let task_ctx = spawner_ctx.clone();
        let handle = tokio::spawn(run_subagent_task(task_ctx, id, spec, update_tx));
        handle.abort_handle()
    });
    agents::Registry::spawn_with_channels(agent_registry_rx, completions_tx, spawner);

    tokio::spawn(run_agent_loop(runner_ctx, agent_rx, completions_rx));

    let state = AppState {
        project: env.project,
        workflow: env.workflow,
        session: env.session,
        issuer: env.project_pubkey,
        state_dir: env.state_dir,
        resolver,
        events,
        next_turn,
        agent_cmds,
        runner_failure: runner_failure_rx,
        agent_registry: agent_registry_tx,
    };

    let idle = lutin_workflow_sdk::idle::IdleTracker::new();
    let idle_watcher = idle.clone();
    tokio::spawn(async move {
        lutin_workflow_sdk::idle::wait_until_idle(idle_watcher, IDLE_TIMEOUT).await;
        info!(timeout_secs = IDLE_TIMEOUT.as_secs(), "idle timeout reached; exiting");
        std::process::exit(0);
    });

    loop {
        let (sock, peer) = listener.accept().await?;
        let state = state.clone();
        let guard = idle.guard();
        tokio::spawn(async move {
            let _guard = guard;
            if let Err(e) = serve_conn(sock, state).await {
                warn!(%peer, error = %e, "connection ended");
            }
        });
    }
}

const IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
