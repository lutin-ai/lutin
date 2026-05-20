//! Reviewed workflow engine binary.
//!
//! One subprocess per session. Same env-handoff convention as the other
//! workflows: bind a loopback listener, publish the bound addr to
//! `LUTIN_WORKFLOW_HANDOFF_PATH`, serve WebSocket connections protected
//! by `WorkflowSession`-scoped tokens.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use anyhow::{Context, Result, anyhow};
use lutin_auth::{SessionId, Slug, VerifyingKey, WorkflowId, pubkey_from_str};
use lutin_storage::Resolver;
use reviewed::runner::{RunnerCtx, run_agent_loop};
use reviewed::serve::{AppState, serve_conn};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc};
use tracing::{info, warn};

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
    info!(%bound, session = %env.session, "reviewed workflow listening");

    lutin_keypair::write_atomic(&env.handoff_path, format!("{bound}\n").as_bytes(), 0o600)
        .with_context(|| format!("write handoff {}", env.handoff_path.display()))?;

    let (events, _) = broadcast::channel(256);
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let resolver = Arc::new(Resolver::new(
        env.global_config_dir.clone(),
        Some(env.project_config_dir.clone()),
    ));
    let runner_ctx = RunnerCtx {
        state_dir: env.state_dir.clone(),
        project_config_dir: env.project_config_dir.clone(),
        resolver: resolver.clone(),
        events: events.clone(),
    };
    tokio::spawn(run_agent_loop(runner_ctx, cmd_rx));

    let state = AppState {
        project: env.project,
        workflow: env.workflow,
        session: env.session,
        issuer: env.project_pubkey,
        state_dir: env.state_dir,
        resolver: resolver.clone(),
        events,
        next_turn: Arc::new(AtomicU64::new(0)),
        agent_cmds: cmd_tx,
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
                warn!(%peer, error = %e, "connection ended with error");
            }
        });
    }
}

const IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
