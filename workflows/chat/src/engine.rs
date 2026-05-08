//! Chat workflow engine binary.
//!
//! One subprocess per session. Spawned by `lutin-project` with the env
//! handoff documented under `Env`. Binds a loopback TCP listener,
//! publishes its bound addr to `LUTIN_WORKFLOW_HANDOFF_PATH` so the
//! supervisor can hand it to clients, then serves WebSocket
//! connections protected by `WorkflowSession`-scoped tokens issued by
//! the project tier.
//!
//! Step 9c lands the real agent loop: each `SendMessage` reloads the
//! persona + settings, builds a fresh provider, and drives an
//! `lutin_agent_sdk::Agent` through one round-loop, mapping the SDK's
//! events onto `ChatEvent` broadcasts.

mod agents;
mod metrics;
mod tools;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use chat::{
    ChatError, ChatEvent, ChatOk, ChatRequest, ChatResponse, FinishReason, HistoricalMessage,
    MessageMeta, SessionState, ToolOutcome, TurnId, decode as chat_decode, encode as chat_encode,
    load_state, save_state,
};
use crate::metrics::{
    AssistantStats, MetricsSidecar, StoredMeta, ToolStats, now_rfc3339,
};
use futures_util::{SinkExt, StreamExt, stream::SplitSink};
use lutin_agent_sdk::{
    Agent, AgentEvent, FinishReason as AgentFinishReason, ToolResult,
};
use lutin_auth::{Scope, SessionId, Slug, VerifyingKey, WorkflowId, pubkey_from_str, verify};
use lutin_entities::Persona;
use lutin_protocol::{Frame, HandshakeResult, PROTOCOL_VERSION, decode, encode};
use lutin_settings::Settings;
use lutin_storage::Resolver;
use lutin_workflow_sdk::agent::{
    build_agent as sdk_build_agent, refresh_agent as sdk_refresh_agent, BuildArgs, BuildError,
};
use lutin_workflow_sdk::transcript;
use serde::Serialize;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};

const DEFAULT_PERSONA: &str = "assistant";

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

/// Commands the WS handlers send to the singleton agent runner task.
/// `Cancel` interrupts an in-flight turn (and is a no-op when idle);
/// `Send` enqueues a new turn (queued behind any in-flight one).
enum AgentCmd {
    Send { text: String, turn: TurnId },
    Rerun { turn: TurnId },
    Cancel,
    /// In-place mutation of the transcript. The new state is delivered
    /// via the `HistoryReplaced` broadcast (single source of truth for
    /// every subscriber, including the originator); the `reply` here
    /// just carries success/failure for the request/response pair.
    Mutate {
        op: MutateOp,
        reply: oneshot::Sender<Result<(), ChatError>>,
    },
}

enum MutateOp {
    Edit { index: u32, text: String },
    Delete { index: u32 },
    DeleteFrom { index: u32 },
}

#[derive(Clone)]
struct AppState {
    project: Slug,
    workflow: WorkflowId,
    session: SessionId,
    issuer: VerifyingKey,
    state_dir: PathBuf,
    /// Needed by request handlers that read shared config (personas,
    /// settings) — not just the agent runner.
    global_config_dir: PathBuf,
    project_config_dir: PathBuf,
    events: broadcast::Sender<ChatEvent>,
    next_turn: Arc<AtomicU64>,
    /// Send-only handle to the agent runner. The runner owns the
    /// `Agent` on its task's stack — there is no shared mutable
    /// agent state in the WS layer.
    agent_cmds: mpsc::UnboundedSender<AgentCmd>,
    /// Set by the runner when it bails (startup failure, panic in
    /// init, etc). Read by `handle_request` when `agent_cmds.send`
    /// returns `Err`, so the user sees the actual reason instead of
    /// a generic "agent runner is gone".
    runner_failure: Arc<Mutex<Option<String>>>,
}

impl AppState {
    fn next_turn(&self) -> TurnId {
        TurnId(self.next_turn.fetch_add(1, Ordering::Relaxed))
    }
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

    // Publish addr atomically so the supervisor's poller observes a
    // complete file on first read.
    lutin_keypair::write_atomic(&env.handoff_path, format!("{bound}\n").as_bytes(), 0o600)
        .with_context(|| format!("write handoff {}", env.handoff_path.display()))?;

    let (events, _) = broadcast::channel(64);
    let (agent_cmds, agent_rx) = mpsc::unbounded_channel();
    let runner_failure: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let next_turn = Arc::new(AtomicU64::new(1));

    // Pre-create the registry's command + completion channels so we
    // can hand `agent_registry` to `RunnerCtx` (which the spawner
    // captures) before the actor is launched. Build order:
    //
    //   1. allocate channel ends
    //   2. construct RunnerCtx with `agent_registry = cmd_tx.clone()`
    //   3. clone RunnerCtx into the spawner closure
    //   4. hand the receiving ends to `Registry::spawn_with_channels`
    let (agent_registry_tx, agent_registry_rx) = mpsc::unbounded_channel();
    let (completions_tx, completions_rx) = mpsc::unbounded_channel();

    let runner_ctx = RunnerCtx {
        state_dir: env.state_dir.clone(),
        global_config_dir: env.global_config_dir.clone(),
        project_config_dir: env.project_config_dir.clone(),
        events: events.clone(),
        failure: runner_failure.clone(),
        next_turn: next_turn.clone(),
        agent_registry: agent_registry_tx,
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
        global_config_dir: env.global_config_dir,
        project_config_dir: env.project_config_dir,
        events,
        next_turn,
        agent_cmds,
        runner_failure,
    };

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

type WsSink = SplitSink<WebSocketStream<TcpStream>, Message>;

async fn send_nack(tx: &mut WsSink, reason: &str) -> Result<()> {
    let nack = encode(&Frame::HelloAck(HandshakeResult::Rejected {
        reason: reason.to_string(),
    }))?;
    tx.send(Message::Binary(nack.into())).await?;
    Ok(())
}

async fn serve_conn(sock: TcpStream, state: AppState) -> Result<()> {
    let ws = tokio_tungstenite::accept_async(sock).await?;
    let (mut tx, mut rx) = ws.split();

    // Hello.
    let Some(msg) = rx.next().await else {
        return Ok(());
    };
    let bytes = match msg? {
        Message::Binary(b) => b,
        _ => anyhow::bail!("expected binary hello"),
    };
    let Frame::Hello {
        protocol_version,
        token,
    } = decode(&bytes)?
    else {
        anyhow::bail!("expected Hello");
    };
    if protocol_version != PROTOCOL_VERSION {
        return send_nack(
            &mut tx,
            &format!(
                "protocol version mismatch: server={PROTOCOL_VERSION} client={protocol_version}"
            ),
        )
        .await;
    }
    match verify(&token, &state.issuer) {
        Ok(claims) => match &claims.scope {
            Scope::WorkflowSession {
                project,
                workflow,
                session,
            } if project == &state.project
                && workflow == &state.workflow
                && session == &state.session => {}
            _ => {
                return send_nack(&mut tx, "scope mismatch for this workflow session").await;
            }
        },
        Err(e) => {
            return send_nack(&mut tx, &format!("auth: {e}")).await;
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
                    let body = chat_encode(&e)?;
                    let frame = encode(&Frame::Broadcast { body })?;
                    if tx.send(Message::Binary(frame.into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(n, "client lagged events; closing connection");
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
                        let req = chat_decode::<ChatRequest>(&body)?;
                        let resp = handle_request(&state, req).await;
                        let body = chat_encode(&resp)?;
                        let out = encode(&Frame::Payload { request_id, body })?;
                        tx.send(Message::Binary(out.into())).await?;
                    }
                    Frame::Ping { nonce } => {
                        let out = encode(&Frame::Pong { nonce })?;
                        tx.send(Message::Binary(out.into())).await?;
                    }
                    Frame::Close { .. } => break,
                    frame => warn!(?frame, "unexpected frame from client"),
                }
            }
        }
    }
    Ok(())
}

async fn handle_request(state: &AppState, req: ChatRequest) -> ChatResponse {
    match req {
        ChatRequest::Subscribe => {
            let s = load_state(&state.state_dir)
                .map_err(|e| ChatError::Internal(format!("load state: {e}")))?;
            let messages = transcript::load(&state.state_dir)
                .map_err(|e| ChatError::Internal(format!("load transcript: {e}")))?;
            Ok(ChatOk::Subscribed {
                state: s,
                history: project_history(&messages),
            })
        }
        ChatRequest::GetState => match load_state(&state.state_dir) {
            Ok(s) => Ok(ChatOk::State(s)),
            Err(e) => Err(ChatError::Internal(format!("load state: {e}"))),
        },
        ChatRequest::SetPersona { name } => {
            let loaded = match load_state(&state.state_dir) {
                Ok(s) => s,
                Err(e) => return Err(ChatError::Internal(format!("load state: {e}"))),
            };
            let s = SessionState { persona: name, ..loaded };
            if let Err(e) = save_state(&state.state_dir, &s) {
                return Err(ChatError::Internal(format!("save state: {e}")));
            }
            let _ = state.events.send(ChatEvent::StateChanged(s.clone()));
            Ok(ChatOk::StateUpdated { state: s })
        }
        ChatRequest::Cancel => {
            let _ = state.agent_cmds.send(AgentCmd::Cancel);
            Ok(ChatOk::Cancelled)
        }
        ChatRequest::ListPersonas => {
            let resolver = Resolver::new(
                state.global_config_dir.clone(),
                Some(state.project_config_dir.clone()),
            );
            let personas = Persona::list(&resolver)
                .map_err(|e| ChatError::Internal(format!("list personas: {e}")))?;
            let projected = personas
                .into_iter()
                .map(|p| chat::PersonaInfo {
                    name: p.name,
                    display_name: p.display_name,
                    model: p.model.unwrap_or_default(),
                })
                .collect();
            Ok(ChatOk::Personas { personas: projected })
        }
        ChatRequest::SendMessage { text } => {
            let turn = state.next_turn();
            if state
                .agent_cmds
                .send(AgentCmd::Send { text, turn })
                .is_err()
            {
                let reason = state
                    .runner_failure
                    .lock()
                    .ok()
                    .and_then(|g| g.clone())
                    .unwrap_or_else(|| "agent runner exited without recording a reason".into());
                return Err(ChatError::Internal(format!("agent runner unavailable: {reason}")));
            }
            Ok(ChatOk::MessageQueued { turn_id: turn })
        }
        ChatRequest::Rerun => {
            let turn = state.next_turn();
            if state.agent_cmds.send(AgentCmd::Rerun { turn }).is_err() {
                let reason = state
                    .runner_failure
                    .lock()
                    .ok()
                    .and_then(|g| g.clone())
                    .unwrap_or_else(|| "agent runner exited without recording a reason".into());
                return Err(ChatError::Internal(format!("agent runner unavailable: {reason}")));
            }
            Ok(ChatOk::MessageQueued { turn_id: turn })
        }
        ChatRequest::EditMessage { index, text } => {
            mutate_via_runner(state, MutateOp::Edit { index, text })
                .await
                .map(|()| ChatOk::HistoryAcknowledged)
        }
        ChatRequest::DeleteMessage { index } => {
            mutate_via_runner(state, MutateOp::Delete { index })
                .await
                .map(|()| ChatOk::HistoryAcknowledged)
        }
        ChatRequest::DeleteFromHere { index } => {
            mutate_via_runner(state, MutateOp::DeleteFrom { index })
                .await
                .map(|()| ChatOk::HistoryAcknowledged)
        }
        ChatRequest::GetMetrics => {
            // Disk-backed read so this works whether or not the runner
            // has booted an Agent yet. Length parity between transcript
            // and sidecar is maintained by every write path that
            // touches one or the other.
            let messages = transcript::load(&state.state_dir)
                .map_err(|e| ChatError::Internal(format!("load transcript: {e}")))?;
            let sidecar = metrics::load(&state.state_dir)
                .map_err(|e| ChatError::Internal(format!("load metrics: {e}")))?;
            Ok(ChatOk::Metrics(project_metrics(&messages, &sidecar)))
        }
    }
}

async fn mutate_via_runner(state: &AppState, op: MutateOp) -> Result<(), ChatError> {
    let (tx, rx) = oneshot::channel();
    if state.agent_cmds.send(AgentCmd::Mutate { op, reply: tx }).is_err() {
        let reason = state
            .runner_failure
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .unwrap_or_else(|| "agent runner exited without recording a reason".into());
        return Err(ChatError::Internal(format!("agent runner unavailable: {reason}")));
    }
    rx.await
        .map_err(|_| ChatError::Internal("runner dropped mutation reply".into()))?
}

/// In-memory bookkeeping for a single turn's metrics. Reset before each
/// `run_turn` invocation, harvested into the sidecar after the turn ends.
#[derive(Default)]
struct TurnTracker {
    /// Wall-clock start of the turn (after the user push, before
    /// `agent.start()`).
    started_at: Option<Instant>,
    /// First `AssistantText` delta in the turn — drives TTFT.
    first_text_at: Option<Instant>,
    /// First `AssistantReasoning` delta in the turn (if any).
    first_thinking_at: Option<Instant>,
    /// Final usage as reported by the provider on the last round.
    last_usage: Option<lutin_llm::Usage>,
    /// Tool-call start times keyed by `ToolCall.id`.
    tool_started: HashMap<String, Instant>,
}

impl TurnTracker {
    fn note_text(&mut self) {
        if self.first_text_at.is_none() {
            self.first_text_at = Some(Instant::now());
        }
    }
    fn note_reasoning(&mut self) {
        if self.first_thinking_at.is_none() {
            self.first_thinking_at = Some(Instant::now());
        }
    }
}

// --- Agent runner ----------------------------------------------------------
//
// Single tokio task owns the `Agent` for the lifetime of one turn. It reads
// commands off `mpsc::UnboundedReceiver` in two states:
//
// * Idle — `recv().await` for the next command. `Cancel` while idle is a
//   no-op (cancellation has nothing to act on).
// * Running — concurrently selects on the agent's event stream and the same
//   command channel. `Cancel` calls `agent.cancel()`; further `Send`
//   commands stay buffered in the channel and are picked up by the next
//   idle iteration. No locks, no shared `Agent`.

#[derive(Clone)]
struct RunnerCtx {
    state_dir: PathBuf,
    global_config_dir: PathBuf,
    project_config_dir: PathBuf,
    events: broadcast::Sender<ChatEvent>,
    /// Shared with `AppState`. Runner writes the failure reason here
    /// before exiting so subsequent `Send` requests can return it
    /// instead of the placeholder "agent runner is gone".
    failure: Arc<Mutex<Option<String>>>,
    /// Shared with `AppState`. Runner allocates fresh `TurnId`s for
    /// auto-turns triggered by sub-agent completions, drawing from the
    /// same monotonic source as user-driven turns so ids stay unique.
    next_turn: Arc<AtomicU64>,
    /// Sender into the sub-agent registry actor. Held here (not just
    /// in `AppState`) because the spawner closure clones a `RunnerCtx`
    /// into each child task, and step-8 tools instantiated during
    /// `build_subagent` will close over this same sender to let the
    /// child spawn its own grandchildren. `run_turn` also reads
    /// `Snapshot` through it for the `<active_subagents>` system-prompt
    /// block.
    agent_registry: mpsc::UnboundedSender<agents::AgentRegistryCmd>,
}

impl RunnerCtx {
    fn next_turn(&self) -> TurnId {
        TurnId(self.next_turn.fetch_add(1, Ordering::Relaxed))
    }

    fn record_failure(&self, reason: impl Into<String>) {
        let reason = reason.into();
        warn!(error = %reason, "agent runner bailing");
        if let Ok(mut slot) = self.failure.lock() {
            // First-write wins — later transient errors after the
            // initial failure shouldn't overwrite the root cause.
            if slot.is_none() {
                *slot = Some(reason);
            }
        }
    }
}

async fn run_agent_loop(
    ctx: RunnerCtx,
    mut rx: mpsc::UnboundedReceiver<AgentCmd>,
    mut completions_rx: mpsc::UnboundedReceiver<agents::CompletionEvent>,
) {
    // The Agent owns the in-memory transcript for the workflow's
    // lifetime. We build it lazily on the first `Send` so that a
    // misconfigured persona (e.g. missing provider) at session-open
    // doesn't kill the runner — the user can `SetPersona` and retry,
    // and we rebuild against the new state. Per-turn refresh via
    // `sdk_refresh_agent` then keeps later out-of-band TOML edits
    // applied without a full rebuild.
    //
    // Truly fatal startup errors (corrupt transcript) still bail —
    // there's no recovery path for those.
    let history = match transcript::load(&ctx.state_dir) {
        Ok(m) => m,
        Err(e) => {
            ctx.record_failure(format!("load transcript: {e}"));
            return;
        }
    };
    // Refresh summary.json on boot so a resumed session gets its
    // last_activity bumped (and a freshly-created session gets a
    // file at all, even before the first turn). Done from the
    // transcript directly so we don't need an Agent for this.
    write_summary(&ctx.state_dir, &history);

    // Drop the startup-loaded transcript once the boot summary is
    // written: from here on the canonical store is the agent (when
    // built) or disk (until then). Re-loading from disk inside
    // `build_initial_agent` keeps that single-source-of-truth even
    // when mutations land before the first turn.
    drop(history);

    // Sub-agent registry was launched in `main()` (the spawner closure
    // there captures a clone of this `ctx`, so children build with the
    // same paths and the same `agent_registry` handle for grandchild
    // spawns). The runner only owns `completions_rx` here, draining
    // terminal events into the auto-turn handler.

    let mut agent: Option<Agent> = None;
    // Metrics sidecar lives in memory for fast updates; persisted to
    // `<state_dir>/metrics.json` alongside the transcript.
    let mut sidecar = metrics::load(&ctx.state_dir).unwrap_or_else(|e| {
        warn!(error = %e, "load metrics sidecar; starting fresh");
        MetricsSidecar::default()
    });
    loop {
        tokio::select! {
            cmd = rx.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    AgentCmd::Send { text, turn } => {
                        if let Some(a) = ensure_agent(&mut agent, &ctx, turn) {
                            run_turn(&ctx, &mut rx, a, &mut sidecar, Some(text), turn).await;
                        }
                    }
                    AgentCmd::Rerun { turn } => {
                        if let Some(a) = ensure_agent(&mut agent, &ctx, turn) {
                            run_turn(&ctx, &mut rx, a, &mut sidecar, None, turn).await;
                        }
                    }
                    AgentCmd::Cancel => {} // idle — nothing to cancel
                    AgentCmd::Mutate { op, reply } => {
                        let result = apply_mutation(&ctx, agent.as_mut(), &mut sidecar, op);
                        let _ = reply.send(result);
                    }
                }
            }
            evt = completions_rx.recv() => match evt {
                Some(evt) => {
                    handle_subagent_completion(&ctx, &mut rx, &mut agent, &mut sidecar, evt).await;
                }
                None => {
                    // Registry actor is gone — only happens at shutdown
                    // when its `cmd_tx` is dropped. Treat as benign.
                }
            }
        }
    }
}

/// Drive one sub-agent run to completion, translating SDK events into
/// `AgentUpdate`s on `update_tx`. Cancellation is via `AbortHandle` on
/// the outer task — the registry's `Stop` aborts us mid-poll, so we
/// don't observe `FinishReason::Cancelled` here (it's only reachable
/// when a future revision wires `agent.cancel()` into the cancel path).
async fn run_subagent_task(
    ctx: RunnerCtx,
    id: agents::AgentId,
    spec: agents::AgentSpec,
    update_tx: mpsc::UnboundedSender<agents::AgentUpdate>,
) {
    let mut agent = match build_subagent(&ctx, &spec) {
        Ok(a) => a,
        Err(reason) => {
            let _ = update_tx.send(agents::AgentUpdate::Failed { id, error: reason });
            return;
        }
    };
    let mut stream = match agent.start() {
        Ok(s) => s,
        Err(e) => {
            let _ = update_tx.send(agents::AgentUpdate::Failed {
                id,
                error: format!("start: {e}"),
            });
            return;
        }
    };
    while let Some(ev) = stream.next().await {
        match ev {
            AgentEvent::AssistantText(s) => {
                let _ = update_tx.send(agents::AgentUpdate::Progress { id, last_text: s });
            }
            AgentEvent::Error(e) => {
                let _ = update_tx.send(agents::AgentUpdate::Failed {
                    id,
                    error: format!("{e}"),
                });
                return;
            }
            AgentEvent::Finished(_) => break,
            _ => {}
        }
    }
    let outcome = agent.join().await;
    match outcome.finish_reason {
        AgentFinishReason::Stopped | AgentFinishReason::MaxRounds => {
            // `last_assistant` is the final assistant turn; pull its
            // text out as the child's deliverable. An empty/None
            // last-assistant means the run ended without producing
            // text (rare — e.g. tool-only final round) — surface as
            // empty string rather than failing.
            let final_text = outcome
                .last_assistant
                .as_ref()
                .and_then(|m| match m {
                    lutin_llm::Message::Assistant { text, .. } => Some(text.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            let _ = update_tx.send(agents::AgentUpdate::Completed {
                id,
                outcome: agents::AgentOutcome { final_text },
            });
        }
        other => {
            // `Cancelled` is unreachable from the registry's `Stop`
            // path (abort kills this task before join). Anything else
            // (`LoopDetected`, `Error`, a future SDK variant) lands
            // here and surfaces as Failed — empty arms would let the
            // slot stay `Running` forever once the task exited.
            let _ = update_tx.send(agents::AgentUpdate::Failed {
                id,
                error: format!("{other:?}"),
            });
        }
    }
}

/// Query the registry for a snapshot and render an
/// `<active_subagents>` system-prompt block. Returns `None` when the
/// registry is empty or unreachable — both cases mean "no block to
/// inject" rather than a hard error: a missing or dropped registry is
/// indistinguishable from "no children spawned" from the LLM's POV.
///
/// Format is line-per-agent for easy LLM parsing; terminal entries
/// (Completed/Failed/Stopped) are kept so the orchestrator has
/// audit-trail context, not just live status.
async fn subagent_block(ctx: &RunnerCtx) -> Option<String> {
    let (tx, rx) = oneshot::channel();
    if ctx
        .agent_registry
        .send(agents::AgentRegistryCmd::Snapshot { reply: tx })
        .is_err()
    {
        return None;
    }
    let summaries = rx.await.ok()?;
    if summaries.is_empty() {
        return None;
    }
    let mut out = String::from("<active_subagents>\n");
    for s in &summaries {
        out.push_str("- ");
        out.push_str(&s.id.to_string());
        out.push_str(" status=");
        match &s.status {
            agents::AgentStatus::Running => out.push_str("running"),
            agents::AgentStatus::Completed => out.push_str("completed"),
            agents::AgentStatus::Failed { reason } => {
                out.push_str(&format!("failed reason={reason:?}"));
            }
            agents::AgentStatus::Stopped => out.push_str("stopped"),
        }
        if let Some(p) = &s.last_progress {
            out.push_str(&format!(" progress={p:?}"));
        }
        out.push('\n');
    }
    out.push_str("</active_subagents>");
    Some(out)
}

/// Append a sub-agent's terminal result to the parent transcript and
/// kick an auto-turn so the parent's LLM gets to react. Runs from the
/// runner's outer `select!` only — never from inside `run_turn` — so
/// the "queue mid-turn completions, fire after current turn" rule is
/// enforced naturally by tokio's task scheduling: while `run_turn` is
/// awaiting in the `Send`/`Rerun` arm, this arm can't progress, and
/// completions accumulate on `completions_rx` until it does.
async fn handle_subagent_completion(
    ctx: &RunnerCtx,
    rx: &mut mpsc::UnboundedReceiver<AgentCmd>,
    agent: &mut Option<Agent>,
    sidecar: &mut MetricsSidecar,
    evt: agents::CompletionEvent,
) {
    let turn = ctx.next_turn();
    // ensure_agent fabricates a `MessageFinished{Failed}` on its own
    // when the build fails, so any UI watching the auto-turn sees a
    // terminal event even if we bail before run_turn.
    let Some(a) = ensure_agent(agent, ctx, turn) else {
        return;
    };
    // Provider serializers wrap each variant as a user-role turn with
    // `[agent#N response]` / `[agent#N failed: …]` framing on the wire;
    // the chat UI uses the structured fields directly.
    let msg = match &evt {
        agents::CompletionEvent::Completed { id, outcome } => lutin_llm::Message::SubAgentReply {
            agent_id: id.to_string(),
            text: outcome.final_text.clone(),
        },
        agents::CompletionEvent::Failed { id, error } => lutin_llm::Message::SubAgentFailure {
            agent_id: id.to_string(),
            reason: error.clone(),
        },
    };
    if let Err(e) = a.push_message(msg) {
        warn!(error = %e, "push agent response failed; skipping auto-turn");
        return;
    }
    // Stamp the just-pushed message with the current time so the UI's
    // metrics footer reflects when the parent saw the response, not
    // when it was first generated upstream.
    while sidecar.messages.len() < a.messages().len() {
        sidecar.messages.push(StoredMeta {
            timestamp: now_rfc3339(),
            ..Default::default()
        });
    }
    // Persist + broadcast before the turn streams so subscribers see
    // the new entry alongside (or before) any assistant deltas. The
    // turn's tail does its own save — this earlier write is the cost
    // of giving the chrome a HistoryReplaced anchor for the injected
    // message.
    if let Err(e) = transcript::save(&ctx.state_dir, a.messages()) {
        warn!(error = %e, "save transcript after agent response failed");
    }
    save_sidecar(&ctx.state_dir, sidecar);
    write_summary(&ctx.state_dir, a.messages());
    broadcast_history_and_metrics(ctx, a.messages(), sidecar);
    // text=None: the agent-response message is already on the
    // transcript; we just want the agent loop to take a turn against it.
    run_turn(ctx, rx, a, sidecar, None, turn).await;
}

/// Apply one mutation op to the canonical history. When the agent
/// exists we mutate its in-memory transcript via `edit_messages`;
/// otherwise we round-trip through disk. Either way we save and
/// broadcast `HistoryReplaced` so every subscriber rerenders.
fn apply_mutation(
    ctx: &RunnerCtx,
    agent: Option<&mut Agent>,
    sidecar: &mut MetricsSidecar,
    op: MutateOp,
) -> Result<(), ChatError> {
    let outcome = if let Some(a) = agent {
        let mut applied: Result<(), ChatError> = Ok(());
        // The SDK rejects edits while a turn is streaming. That's
        // exactly `TurnInFlight` — surface it specifically so the UI
        // can disable the menu rather than show a generic "internal".
        a.edit_messages(|msgs| {
            applied = mutate_messages_with_meta(msgs, &mut sidecar.messages, &op);
        })
        .map_err(|_| ChatError::TurnInFlight)?;
        applied?;
        a.messages().to_vec()
    } else {
        let mut messages = transcript::load(&ctx.state_dir)
            .map_err(|e| ChatError::Internal(format!("load transcript: {e}")))?;
        mutate_messages_with_meta(&mut messages, &mut sidecar.messages, &op)?;
        messages
    };
    // Mutation already mutated in-memory state; if disk persistence
    // fails the user needs to know — silently warning would leave the
    // chat acknowledged-as-applied while the next session-restart
    // would resurrect the pre-mutation transcript.
    transcript::save(&ctx.state_dir, &outcome)
        .map_err(|e| {
            warn!(error = %e, "save transcript after mutation failed");
            ChatError::PersistFailed { op: "save transcript".into() }
        })?;
    metrics::save(&ctx.state_dir, sidecar).map_err(|e| {
        warn!(error = %e, "save metrics sidecar after mutation failed");
        ChatError::PersistFailed { op: "save metrics".into() }
    })?;
    write_summary(&ctx.state_dir, &outcome);
    broadcast_history_and_metrics(ctx, &outcome, sidecar);
    Ok(())
}

fn mutate_messages_with_meta(
    messages: &mut Vec<lutin_llm::Message>,
    meta: &mut Vec<StoredMeta>,
    op: &MutateOp,
) -> Result<(), ChatError> {
    // Snapshot the underlying message-index of the projected entry
    // *before* mutation, so we can keep the parallel meta vec aligned
    // when an underlying message is removed/truncated.
    let target_message_index = match op {
        MutateOp::Edit { index, .. } => Some(locate(messages, *index)?.0),
        MutateOp::Delete { index } => Some(locate(messages, *index)?.0),
        MutateOp::DeleteFrom { index } => Some(locate(messages, *index)?.0),
    };
    let prev_len = messages.len();
    let prev_was_user = target_message_index
        .and_then(|mi| messages.get(mi))
        .map(|m| matches!(m, lutin_llm::Message::User(t) if !t.is_empty()))
        .unwrap_or(false);
    mutate_messages(messages, op)?;
    // Re-align meta: if a Message was removed (User delete) or the tail
    // was truncated (DeleteFrom), drop the same indices from `meta`.
    match op {
        MutateOp::Delete { .. } if prev_was_user => {
            if let Some(mi) = target_message_index
                && mi < meta.len()
            {
                meta.remove(mi);
            }
        }
        MutateOp::DeleteFrom { .. } => {
            if let Some(mi) = target_message_index {
                meta.truncate(mi);
            }
        }
        _ => {}
    }
    let _ = prev_len; // suppress unused-warning; kept for potential future invariants
    Ok(())
}

fn save_sidecar(state_dir: &std::path::Path, sidecar: &MetricsSidecar) {
    if let Err(e) = metrics::save(state_dir, sidecar) {
        warn!(error = %e, "save metrics sidecar failed");
    }
}

fn broadcast_history_and_metrics(
    ctx: &RunnerCtx,
    messages: &[lutin_llm::Message],
    sidecar: &MetricsSidecar,
) {
    let history = project_history(messages);
    let metrics_proj = project_metrics(messages, sidecar);
    let _ = ctx.events.send(ChatEvent::HistoryReplaced(history));
    let _ = ctx.events.send(ChatEvent::MetricsReplaced(metrics_proj));
}

/// Which field of an underlying `Message` a projected history entry
/// addresses. The projection in `project_history` emits one entry per
/// non-empty `User`, one per non-empty `Assistant.thinking`, one per
/// non-empty `Assistant.text`, and one per `Assistant.tool_calls`
/// entry; this enum names those slots so `mutate_messages` can
/// dispatch without re-walking the variants.
#[derive(Debug, Clone, Copy)]
enum ProjectedSlot {
    User,
    Thinking,
    AssistantText,
    /// One entry per `Assistant.tool_calls[idx]`. Rejected by
    /// `mutate_messages` — tool exchanges aren't user-editable. Kept in
    /// the iterator so projected indices line up with the wire.
    Tool,
    /// One `Message::SubAgentReply` or `Message::SubAgentFailure`.
    /// Rejected by `mutate_messages` — sub-agent turns are produced
    /// upstream, not user-authored.
    SubAgent,
}

/// Walk `messages` in projected order, yielding `(message_index, slot)`
/// for each visible entry. Mirrors `project_history` exactly so
/// projected indices line up between the wire and the runner.
fn projected_slots(
    messages: &[lutin_llm::Message],
) -> impl Iterator<Item = (usize, ProjectedSlot)> + '_ {
    use lutin_llm::Message;
    messages.iter().enumerate().flat_map(|(i, m)| {
        // Each arm produces a 0..N slot iterator without heap allocation.
        // The non-Assistant arms are at most 1 slot, so a fixed-size
        // array works; Assistant chains thinking + text + tool_calls.
        let user = matches!(m, Message::User(t) if !t.is_empty())
            .then_some(ProjectedSlot::User);
        let sub_agent = matches!(
            m,
            Message::SubAgentReply { .. } | Message::SubAgentFailure { .. }
        )
        .then_some(ProjectedSlot::SubAgent);
        let (thinking, text, tools) = match m {
            Message::Assistant { text, thinking, tool_calls } => (
                thinking
                    .as_deref()
                    .is_some_and(|s| !s.is_empty())
                    .then_some(ProjectedSlot::Thinking),
                (!text.is_empty()).then_some(ProjectedSlot::AssistantText),
                Some(tool_calls.as_slice()),
            ),
            _ => (None, None, None),
        };
        user.into_iter()
            .chain(sub_agent)
            .chain(thinking)
            .chain(text)
            .chain(
                tools
                    .into_iter()
                    .flat_map(|tcs| tcs.iter().map(|_| ProjectedSlot::Tool)),
            )
            .map(move |s| (i, s))
    })
}

/// Resolve a projected index to the underlying `(message_index, slot)`,
/// or report it out of range.
fn locate(
    messages: &[lutin_llm::Message],
    index: u32,
) -> Result<(usize, ProjectedSlot), ChatError> {
    projected_slots(messages)
        .nth(index as usize)
        .ok_or(ChatError::HistoryIndexOutOfRange(index))
}

/// In-place mutation of the engine-side `Vec<Message>`. Indices address
/// the same projected history the UI sees; `locate` does the mapping
/// once and we dispatch on the resolved slot.
fn mutate_messages(
    messages: &mut Vec<lutin_llm::Message>,
    op: &MutateOp,
) -> Result<(), ChatError> {
    use lutin_llm::Message;
    match op {
        MutateOp::Edit { index, text } => {
            let (mi, slot) = locate(messages, *index)?;
            // Dispatch on slot first (matching `Delete`'s shape).
            // `locate` already proved the slot lines up with the
            // underlying message variant, so the inner `if let`s are
            // statically guaranteed to bind.
            match slot {
                ProjectedSlot::User => {
                    if let Message::User(t) = &mut messages[mi] {
                        *t = text.clone();
                    }
                }
                ProjectedSlot::Thinking => {
                    if let Message::Assistant { thinking, .. } = &mut messages[mi] {
                        *thinking = Some(text.clone());
                    }
                }
                ProjectedSlot::AssistantText => {
                    if let Message::Assistant { text: at, .. } = &mut messages[mi] {
                        *at = text.clone();
                    }
                }
                // Tool exchanges and sub-agent replies aren't
                // user-editable; surface as out-of-range so the UI can
                // disable the menu item.
                ProjectedSlot::Tool | ProjectedSlot::SubAgent => {
                    return Err(ChatError::HistoryIndexOutOfRange(*index));
                }
            }
            Ok(())
        }
        MutateOp::Delete { index } => {
            let (mi, slot) = locate(messages, *index)?;
            match slot {
                ProjectedSlot::User => {
                    messages.remove(mi);
                }
                ProjectedSlot::Thinking => {
                    if let Message::Assistant { thinking, .. } = &mut messages[mi] {
                        *thinking = None;
                    }
                }
                ProjectedSlot::AssistantText => {
                    if let Message::Assistant { text, .. } = &mut messages[mi] {
                        text.clear();
                    }
                }
                ProjectedSlot::Tool | ProjectedSlot::SubAgent => {
                    return Err(ChatError::HistoryIndexOutOfRange(*index));
                }
            }
            Ok(())
        }
        MutateOp::DeleteFrom { index } => {
            let (mi, _) = locate(messages, *index)?;
            messages.truncate(mi);
            Ok(())
        }
    }
}

/// Lazy-build the agent on first use; surface init failures as a
/// turn-level error so the runner stays alive. Returns `None` when
/// the build failed (and the caller should skip the turn).
fn ensure_agent<'a>(
    slot: &'a mut Option<Agent>,
    ctx: &RunnerCtx,
    turn: TurnId,
) -> Option<&'a mut Agent> {
    if slot.is_none() {
        match build_initial_agent(ctx) {
            Ok(a) => *slot = Some(a),
            Err(reason) => {
                let _ = ctx.events.send(ChatEvent::MessageFinished {
                    turn_id: turn,
                    reason: FinishReason::Failed(reason),
                });
                return None;
            }
        }
    }
    slot.as_mut()
}

/// Build the agent on first use, seeding it from the latest on-disk
/// transcript. Reloading here (rather than carrying a pre-loaded copy
/// from runner startup) keeps disk as the sole pre-build source of
/// truth so any mutations applied before the first turn are picked up.
fn build_initial_agent(ctx: &RunnerCtx) -> Result<Agent, String> {
    let resolved = resolve_args(ctx, None).map_err(|e| format!("resolve args: {e}"))?;
    let history = transcript::load(&ctx.state_dir)
        .map_err(|e| format!("load transcript: {e}"))?;
    let mut agent = sdk_build_agent(resolved.as_build_args(ctx))
        .map_err(|e| format!("build agent: {}", map_build_error(e)))?;
    agent
        .edit_messages(|m| *m = history)
        .map_err(|e| format!("seed agent messages: {e}"))?;
    Ok(agent)
}

/// Build a sub-agent from an [`agents::AgentSpec`]. Uses the parent's
/// settings + sandbox path (re-resolved fresh from disk) but seeds
/// messages from the spec's frozen `Arc<Vec<Message>>` snapshot rather
/// than the live on-disk transcript. The initial user prompt is queued
/// so the caller's `agent.start()` consumes it on the first round.
///
/// `spec.persona` overrides the parent's session persona when set;
/// `None` inherits whatever the parent has chosen at this moment.
///
/// Returns owned errors (not `ChatError`) — sub-agent failures surface
/// to the registry as `AgentUpdate::Failed { error }`, not to the chat
/// protocol layer.
fn build_subagent(ctx: &RunnerCtx, spec: &agents::AgentSpec) -> Result<Agent, String> {
    let resolved = resolve_args(ctx, spec.persona.as_deref())
        .map_err(|e| format!("resolve args: {e}"))?;
    let mut agent = sdk_build_agent(resolved.as_build_args(ctx))
        .map_err(|e| format!("build agent: {}", map_build_error(e)))?;
    let snapshot = (*spec.transcript_snapshot).clone();
    agent
        .edit_messages(|m| *m = snapshot)
        .map_err(|e| format!("seed agent messages: {e}"))?;
    agent
        .push_message(lutin_llm::Message::User(spec.initial_prompt.clone()))
        .map_err(|e| format!("push initial prompt: {e}"))?;
    Ok(agent)
}

/// `text` is `Some(_)` for a new user message and `None` for a Rerun,
/// which kicks the agent loop against the existing transcript.
async fn run_turn(
    ctx: &RunnerCtx,
    rx: &mut mpsc::UnboundedReceiver<AgentCmd>,
    agent: &mut Agent,
    sidecar: &mut MetricsSidecar,
    text: Option<String>,
    turn: TurnId,
) {
    // Refresh provider/model/sampling/system/tools from disk so
    // out-of-band edits to persona or settings take effect on this
    // turn. The agent's `messages` survive the swap.
    let resolved = match resolve_args(ctx, None) {
        Ok(r) => r,
        Err(e) => {
            let _ = ctx.events.send(ChatEvent::MessageFinished {
                turn_id: turn,
                reason: FinishReason::Failed(format!("{e}")),
            });
            return;
        }
    };
    if let Err(e) = sdk_refresh_agent(agent, resolved.as_build_args(ctx)) {
        let _ = ctx.events.send(ChatEvent::MessageFinished {
            turn_id: turn,
            reason: FinishReason::Failed(format!("{}", map_build_error(e))),
        });
        return;
    }
    // Augment the system prompt with a live `<active_subagents>` block
    // (when any are tracked). Done after refresh so the persona's base
    // prompt is the canonical input on every turn — the block is
    // re-derived fresh and never accumulates stale entries across
    // turns.
    if let Some(block) = subagent_block(ctx).await {
        let _ = agent.update_config(|cfg| {
            if cfg.system.is_empty() {
                cfg.system = block;
            } else {
                cfg.system.push_str("\n\n");
                cfg.system.push_str(&block);
            }
        });
    }
    // start() consumes pending messages on this run, so push first
    // (skipped on Rerun, which deliberately runs against the existing
    // transcript without appending a new user message).
    if let Some(text) = text {
        if let Err(e) = agent.push_message(lutin_llm::Message::User(text)) {
            let _ = ctx.events.send(ChatEvent::MessageFinished {
                turn_id: turn,
                reason: FinishReason::Failed(format!("push: {e}")),
            });
            return;
        }
        // Stamp the just-pushed user message and broadcast immediately
        // so the chrome can render the user's bubble (with timestamp)
        // before the assistant starts streaming.
        sidecar.messages.push(StoredMeta {
            timestamp: now_rfc3339(),
            ..Default::default()
        });
        save_sidecar(&ctx.state_dir, sidecar);
        broadcast_history_and_metrics(ctx, agent.messages(), sidecar);
    }
    let pre_turn_len = agent.messages().len();
    let mut tracker = TurnTracker::default();
    tracker.started_at = Some(Instant::now());
    let mut stream = match agent.start() {
        Ok(s) => s,
        Err(e) => {
            let _ = ctx.events.send(ChatEvent::MessageFinished {
                turn_id: turn,
                reason: FinishReason::Failed(format!("start: {e}")),
            });
            return;
        }
    };

    let mut finish: Option<FinishReason> = None;
    loop {
        tokio::select! {
            ev = stream.next() => match ev {
                Some(ev) => {
                    if let Some(reason) = handle_agent_event(ev, &ctx.events, &mut tracker, sidecar) {
                        finish = Some(reason);
                    }
                }
                None => break,
            },
            cmd = rx.recv() => match cmd {
                Some(AgentCmd::Cancel) => agent.cancel(),
                Some(AgentCmd::Send { turn: dropped_turn, .. })
                | Some(AgentCmd::Rerun { turn: dropped_turn }) => {
                    warn!("send/rerun received during in-flight turn — dropping; client should wait");
                    let _ = ctx.events.send(ChatEvent::MessageFinished {
                        turn_id: dropped_turn,
                        reason: FinishReason::Failed("turn already in flight".into()),
                    });
                }
                Some(AgentCmd::Mutate { reply, .. }) => {
                    let _ = reply.send(Err(ChatError::TurnInFlight));
                }
                None => {
                    agent.cancel();
                }
            }
        }
    }

    let outcome = agent.join().await;
    // Harvest turn-level stats for any new messages the agent appended
    // during this turn (typically: 0 or more Assistant rounds + their
    // ToolResults). Attribute Usage and full timing to the FINAL
    // assistant text; intermediates get just a timestamp.
    finalize_turn_meta(agent.messages(), pre_turn_len, &tracker, &mut sidecar.messages);
    // Single write per turn from the agent's own message vec — the
    // single source of truth. Even on Cancel/Failed, partials are
    // preserved so the user can see where it stopped.
    if let Err(e) = transcript::save(&ctx.state_dir, agent.messages()) {
        warn!(error = %e, "save transcript failed");
    }
    save_sidecar(&ctx.state_dir, sidecar);
    write_summary(&ctx.state_dir, agent.messages());
    let reason = finish.unwrap_or_else(|| map_finish_reason(outcome.finish_reason));
    let _ = ctx.events.send(ChatEvent::MessageFinished {
        turn_id: turn,
        reason,
    });
    broadcast_history_and_metrics(ctx, agent.messages(), sidecar);
}

/// Walk the messages added during the turn (`pre_turn_len..end`) and
/// attach metadata. The last `Message::Assistant` in the new range
/// gets the full turn-level stats (TTFT, duration, tokens); earlier
/// assistants and `ToolResult` messages only get a timestamp.
fn finalize_turn_meta(
    messages: &[lutin_llm::Message],
    pre_turn_len: usize,
    tracker: &TurnTracker,
    meta: &mut Vec<StoredMeta>,
) {
    // Backfill any meta entries the turn pushed past — for ToolResult
    // and intermediate Assistant messages, just stamp the time.
    while meta.len() < messages.len() {
        meta.push(StoredMeta {
            timestamp: now_rfc3339(),
            ..Default::default()
        });
    }
    // Find the LAST assistant message added this turn; attach turn
    // stats there. If there's none (e.g. cancel before first round
    // produced anything), nothing to do.
    let last_assistant_idx = (pre_turn_len..messages.len())
        .rev()
        .find(|&i| matches!(messages[i], lutin_llm::Message::Assistant { .. }));
    let Some(idx) = last_assistant_idx else {
        return;
    };
    let now = Instant::now();
    let duration_ms = tracker
        .started_at
        .map(|t0| now.saturating_duration_since(t0).as_millis() as u64);
    let ttft_ms = tracker
        .started_at
        .zip(tracker.first_text_at)
        .map(|(t0, t1)| t1.saturating_duration_since(t0).as_millis() as u64);
    let thinking_ttft_ms = tracker
        .started_at
        .zip(tracker.first_thinking_at)
        .map(|(t0, t1)| t1.saturating_duration_since(t0).as_millis() as u64);
    let (prompt_tokens, completion_tokens) = match &tracker.last_usage {
        Some(u) => (Some(u.prompt_tokens), Some(u.completion_tokens)),
        None => (None, None),
    };
    let entry = &mut meta[idx];
    if let lutin_llm::Message::Assistant { text, thinking, .. } = &messages[idx] {
        if !text.is_empty() {
            entry.assistant = Some(AssistantStats {
                ttft_ms,
                duration_ms,
                prompt_tokens,
                completion_tokens,
            });
        }
        if thinking.as_deref().is_some_and(|s| !s.is_empty()) {
            entry.thinking = Some(AssistantStats {
                ttft_ms: thinking_ttft_ms,
                duration_ms,
                prompt_tokens,
                completion_tokens,
            });
        }
    }
}

/// Workflow-supplied summary file CP reads at `ListSessions` time
/// to label this session in the desktop's list. Schema is shared
/// across workflows (chrome reads it identically) — keep it in sync
/// with `lutin_control_protocol::SessionSummary`. We mirror the type
/// rather than depend on the CP crate so chat keeps its lean
/// dependency footprint.
#[derive(Debug, Clone, Serialize)]
struct ChatSummary {
    title: Option<String>,
    subtitle: Option<String>,
    last_activity: Option<String>,
    preview: Option<String>,
}

const SUMMARY_TITLE_CHARS: usize = 80;
const SUMMARY_PREVIEW_CHARS: usize = 160;

/// Build + atomically write `<state_dir>/summary.json`. Called after
/// every turn so the dormant-session label tracks the latest state;
/// also called once at runner startup so the file exists before any
/// turns happen. Failures log a warning but never bubble — a missing
/// summary just means the chrome shows a generic fallback label, not
/// that the session is broken.
fn write_summary(state_dir: &std::path::Path, messages: &[lutin_llm::Message]) {
    let summary = build_summary(messages);
    let payload = match serde_json::to_vec_pretty(&summary) {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "encode summary.json failed");
            return;
        }
    };
    let path = state_dir.join("summary.json");
    let tmp = state_dir.join("summary.json.tmp");
    if let Err(e) = std::fs::write(&tmp, &payload) {
        warn!(error = %e, path = %tmp.display(), "write summary tmp failed");
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        warn!(error = %e, "rename summary tmp into place failed");
    }
}

fn build_summary(messages: &[lutin_llm::Message]) -> ChatSummary {
    let title = messages.iter().find_map(|m| match m {
        lutin_llm::Message::User(text) if !text.trim().is_empty() => {
            Some(truncate_chars(text.trim(), SUMMARY_TITLE_CHARS))
        }
        _ => None,
    });
    let preview = messages.iter().rev().find_map(|m| match m {
        lutin_llm::Message::Assistant { text, .. } if !text.trim().is_empty() => {
            Some(truncate_chars(text.trim(), SUMMARY_PREVIEW_CHARS))
        }
        _ => None,
    });
    let visible = messages
        .iter()
        .filter(|m| {
            matches!(
                m,
                lutin_llm::Message::User(t) if !t.is_empty(),
            ) || matches!(
                m,
                lutin_llm::Message::Assistant { text, .. } if !text.is_empty(),
            )
        })
        .count();
    let subtitle = if visible == 0 {
        None
    } else if visible == 1 {
        Some("1 message".into())
    } else {
        Some(format!("{visible} messages"))
    };
    ChatSummary {
        title,
        subtitle,
        last_activity: Some(chrono::Utc::now().to_rfc3339()),
        preview,
    }
}

/// Char-aware (not byte-aware) truncation, with a single ellipsis
/// when we cut. Avoids splitting multi-byte UTF-8 sequences.
fn truncate_chars(s: &str, max_chars: usize) -> String {
    let mut count = 0;
    let mut end_byte = s.len();
    for (idx, _) in s.char_indices() {
        if count == max_chars {
            end_byte = idx;
            break;
        }
        count += 1;
    }
    if end_byte < s.len() {
        let mut out = s[..end_byte].to_owned();
        out.push('…');
        out
    } else {
        s.to_owned()
    }
}

/// Project the engine's `Vec<Message>` to the wire shape, preserving
/// order. Tool calls are paired with their later `Message::ToolResult`
/// by `call_id`; `projected_slots` mirrors this iteration order.
fn project_history(messages: &[lutin_llm::Message]) -> Vec<HistoricalMessage> {
    // Linear scan to pair tool_calls with their later ToolResult by
    // call_id. Tool counts per turn are typically <10, so a Vec
    // beats a HashMap on both setup cost and lookup time at this scale.
    let mut results_by_id: Vec<(&str, &lutin_llm::ToolResultContent)> = Vec::new();
    for m in messages {
        if let lutin_llm::Message::ToolResult(tr) = m {
            results_by_id.push((tr.call_id.as_str(), tr));
        }
    }
    let mut out = Vec::with_capacity(messages.len());
    for m in messages {
        match m {
            lutin_llm::Message::User(text) if !text.is_empty() => {
                out.push(HistoricalMessage::User(text.clone()));
            }
            lutin_llm::Message::SubAgentReply { agent_id, text } => {
                out.push(HistoricalMessage::SubAgentReply {
                    agent_id: agent_id.clone(),
                    text: text.clone(),
                });
            }
            lutin_llm::Message::SubAgentFailure { agent_id, reason } => {
                out.push(HistoricalMessage::SubAgentFailure {
                    agent_id: agent_id.clone(),
                    reason: reason.clone(),
                });
            }
            lutin_llm::Message::Assistant { text, thinking, tool_calls } => {
                if let Some(t) = thinking
                    && !t.is_empty()
                {
                    out.push(HistoricalMessage::Thinking(t.clone()));
                }
                if !text.is_empty() {
                    out.push(HistoricalMessage::Assistant(text.clone()));
                }
                for call in tool_calls {
                    // `Value` serialization is infallible — `Number`, `String`,
                    // `Bool`, `Null`, `Array`, `Object` all serialize.
                    let arguments_json = serde_json::to_string(&call.arguments)
                        .expect("serializing serde_json::Value is infallible");
                    let outcome = results_by_id
                        .iter()
                        .find(|(id, _)| *id == call.id.as_str())
                        .map(|(_, tr)| {
                            if tr.is_error {
                                ToolOutcome::Failed(tr.content.clone())
                            } else {
                                ToolOutcome::Ok(tr.content.clone())
                            }
                        });
                    out.push(HistoricalMessage::Tool {
                        call_id: call.id.as_str().to_string(),
                        name: call.name.as_str().to_string(),
                        arguments_json,
                        outcome,
                    });
                }
            }
            _ => {}
        }
    }
    out
}

/// Translate one [`AgentEvent`] to zero-or-more [`ChatEvent`]s; returns
/// the terminal `FinishReason` when the agent's run ends.
fn handle_agent_event(
    ev: AgentEvent,
    events: &broadcast::Sender<ChatEvent>,
    tracker: &mut TurnTracker,
    sidecar: &mut MetricsSidecar,
) -> Option<FinishReason> {
    match ev {
        AgentEvent::AssistantText(s) => {
            tracker.note_text();
            let _ = events.send(ChatEvent::Delta(s));
            None
        }
        AgentEvent::AssistantReasoning(s) => {
            tracker.note_reasoning();
            let _ = events.send(ChatEvent::Reasoning(s));
            None
        }
        AgentEvent::ToolCallStarted(call) => {
            tracker
                .tool_started
                .insert(call.id.as_str().to_string(), Instant::now());
            sidecar.tools.insert(
                call.id.as_str().to_string(),
                ToolStats {
                    timestamp: now_rfc3339(),
                    duration_ms: None,
                },
            );
            // `Value` serialization is infallible — every variant has
            // a deterministic textual form. The TS decoder parses the
            // resulting JSON once at the wire boundary so downstream
            // sees a parsed value, not a string.
            let arguments_json = serde_json::to_string(&call.arguments)
                .expect("serializing serde_json::Value is infallible");
            let _ = events.send(ChatEvent::ToolCallStarted {
                id: call.id.as_str().to_string(),
                name: call.name.as_str().to_string(),
                arguments_json,
            });
            None
        }
        AgentEvent::ToolCallCompleted { call, outcome } => {
            if let Some(started) = tracker.tool_started.remove(call.id.as_str()) {
                let dur = Instant::now().saturating_duration_since(started).as_millis() as u64;
                if let Some(stats) = sidecar.tools.get_mut(call.id.as_str()) {
                    stats.duration_ms = Some(dur);
                }
            }
            let chat_outcome = match outcome {
                ToolResult::Ok(c) if c.is_error => ToolOutcome::Failed(c.content),
                ToolResult::Ok(c) => ToolOutcome::Ok(c.content),
                ToolResult::Err(e) => ToolOutcome::Failed(format!("{e}")),
                other => {
                    warn!(?other, "unrecognized ToolResult variant");
                    ToolOutcome::Failed("unrecognized ToolResult variant".to_string())
                }
            };
            let _ = events.send(ChatEvent::ToolCallCompleted {
                id: call.id.as_str().to_string(),
                outcome: chat_outcome,
            });
            None
        }
        AgentEvent::Usage(u) => {
            tracker.last_usage = Some(u);
            None
        }
        AgentEvent::Finished(reason) => Some(map_finish_reason(reason)),
        AgentEvent::Error(e) => Some(FinishReason::Failed(format!("{e}"))),
        AgentEvent::RoundStarted { .. }
        | AgentEvent::RoundEnded { .. }
        | AgentEvent::AssistantMessage(_) => None,
        other => {
            warn!(?other, "unrecognized AgentEvent variant");
            None
        }
    }
}

/// Project the in-memory `MetricsSidecar` to the wire-shape `Vec<MessageMeta>`,
/// aligned to the same iteration order as `project_history`.
fn project_metrics(
    messages: &[lutin_llm::Message],
    sidecar: &MetricsSidecar,
) -> Vec<MessageMeta> {
    let mut out: Vec<MessageMeta> = Vec::with_capacity(messages.len());
    for (i, m) in messages.iter().enumerate() {
        let stored = sidecar.messages.get(i).cloned().unwrap_or_default();
        match m {
            lutin_llm::Message::User(text) if !text.is_empty() => {
                out.push(MessageMeta {
                    timestamp: stored.timestamp,
                    ..Default::default()
                });
            }
            lutin_llm::Message::SubAgentReply { .. } | lutin_llm::Message::SubAgentFailure { .. } => {
                out.push(MessageMeta {
                    timestamp: stored.timestamp,
                    ..Default::default()
                });
            }
            lutin_llm::Message::Assistant { text, thinking, tool_calls } => {
                if thinking.as_deref().is_some_and(|s| !s.is_empty()) {
                    let s = stored.thinking.unwrap_or_default();
                    out.push(MessageMeta {
                        timestamp: stored.timestamp.clone(),
                        ttft_ms: s.ttft_ms,
                        duration_ms: s.duration_ms,
                        prompt_tokens: s.prompt_tokens,
                        completion_tokens: s.completion_tokens,
                    });
                }
                if !text.is_empty() {
                    let s = stored.assistant.unwrap_or_default();
                    out.push(MessageMeta {
                        timestamp: stored.timestamp.clone(),
                        ttft_ms: s.ttft_ms,
                        duration_ms: s.duration_ms,
                        prompt_tokens: s.prompt_tokens,
                        completion_tokens: s.completion_tokens,
                    });
                }
                for call in tool_calls {
                    let tool_meta = sidecar.tools.get(call.id.as_str());
                    out.push(MessageMeta {
                        timestamp: tool_meta
                            .map(|t| t.timestamp.clone())
                            .unwrap_or_default(),
                        duration_ms: tool_meta.and_then(|t| t.duration_ms),
                        ..Default::default()
                    });
                }
            }
            _ => {}
        }
    }
    out
}

/// Translate the SDK's terminal reason to the chat protocol's. Shared
/// between the streaming `Finished` handler and the post-loop join
/// fallback so they can't drift.
fn map_finish_reason(reason: AgentFinishReason) -> FinishReason {
    match reason {
        AgentFinishReason::Stopped | AgentFinishReason::MaxRounds => FinishReason::Completed,
        AgentFinishReason::Cancelled => FinishReason::Cancelled,
        AgentFinishReason::LoopDetected => FinishReason::Failed("loop detected".into()),
        AgentFinishReason::Error(e) => FinishReason::Failed(format!("{e}")),
        other => {
            warn!(?other, "unrecognized AgentFinishReason variant");
            FinishReason::Failed("unrecognized AgentFinishReason".into())
        }
    }
}

/// Owned bundle of inputs the SDK's `BuildArgs` borrows from.
/// Re-resolved per turn so out-of-band edits to `state.toml`,
/// persona TOML, or settings TOML take effect on the next turn.
struct ResolvedArgs {
    persona: Persona,
    settings: Settings,
    sandbox_root: PathBuf,
    model_override: Option<String>,
}

impl ResolvedArgs {
    /// Bind the resolved inputs to the SDK's build interface. Sub-agent
    /// tools are constructed fresh per call (each one closes over a
    /// clone of the registry sender from `ctx`); the persona's filter
    /// then drops them for non-orchestrator personas — see
    /// `tools::agent` for the gating story.
    fn as_build_args(&self, ctx: &RunnerCtx) -> BuildArgs<'_> {
        BuildArgs {
            persona: &self.persona,
            settings: &self.settings,
            sandbox_root: self.sandbox_root.clone(),
            model_override: self.model_override.clone(),
            extra_tools: tools::make_subagent_tools(ctx.agent_registry.clone()),
        }
    }
}

/// Resolve the chat-specific inputs the SDK needs from on-disk state.
/// Translates SDK-agnostic errors (file IO, persona-not-found) back to
/// the chat protocol's typed variants.
///
/// `persona_override` lets sub-agents pick a persona other than the
/// parent session's — when `None`, falls back to `session_state.persona`
/// then to [`DEFAULT_PERSONA`].
fn resolve_args(
    ctx: &RunnerCtx,
    persona_override: Option<&str>,
) -> Result<ResolvedArgs, ChatError> {
    let session_state = load_state(&ctx.state_dir)
        .map_err(|e| ChatError::Internal(format!("load state: {e}")))?;

    let resolver = Resolver::new(
        ctx.global_config_dir.clone(),
        Some(ctx.project_config_dir.clone()),
    );

    let persona_name = persona_override
        .or(session_state.persona.as_deref())
        .unwrap_or(DEFAULT_PERSONA);
    let persona = Persona::load(&resolver, persona_name).map_err(|e| match e {
        lutin_entities::EntityError::NotFound { name, .. } => ChatError::PersonaNotFound(name),
        other => ChatError::Internal(format!("load persona: {other}")),
    })?;
    let settings =
        Settings::load(&resolver).map_err(|e| ChatError::Internal(format!("load settings: {e}")))?;

    // Sandbox root: the project workspace itself, not `.lutin/`. Tools
    // jail filesystem access here so the agent can read/edit user code.
    // `project_config_dir` is `<root>/<slug>/.lutin/`; `parent()` strips
    // the trailing component to give `<root>/<slug>/`. A None parent
    // would mean a root path like `/` or empty — an env-handoff bug
    // that should surface, not silently weaken the sandbox.
    let sandbox_root = ctx
        .project_config_dir
        .parent()
        .ok_or_else(|| {
            ChatError::Internal(format!(
                "project_config_dir has no parent: {}",
                ctx.project_config_dir.display()
            ))
        })?
        .to_path_buf();

    Ok(ResolvedArgs {
        persona,
        settings,
        sandbox_root,
        model_override: session_state.model_override,
    })
}

fn map_build_error(e: BuildError) -> ChatError {
    match e {
        BuildError::ProviderNotFound(n) => ChatError::ProviderNotFound(n),
        BuildError::ProviderMisconfigured { name, reason } => {
            ChatError::ProviderMisconfigured { name, reason }
        }
        BuildError::ProviderUnsupported(s) => ChatError::ProviderUnsupported(s),
        BuildError::PersonaMissingProvider(_)
        | BuildError::PersonaMissingModel(_)
        | BuildError::Toolbox(_) => ChatError::Internal(e.to_string()),
    }
}

