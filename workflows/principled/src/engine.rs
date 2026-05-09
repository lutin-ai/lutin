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
mod principle;
mod review;
mod reviewer;
mod step;
mod store;
mod tools;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use principled::{
    ChatError, ChatEvent, ChatOk, ChatRequest, ChatResponse, FinishReason, HistoricalMessage,
    MessageMeta, SessionState, SubAgentInfo, SubAgentStatus, ToolOutcome, TurnId,
    decode as chat_decode, encode as chat_encode, load_state, save_state,
};
use crate::store::{
    Entry, MessageMetrics, TextStats, ThinkingStats, ToolStats, now_rfc3339,
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
    build_agent as sdk_build_agent, build_provider as sdk_build_provider,
    refresh_agent as sdk_refresh_agent, BuildArgs, BuildError,
};
use lutin_workflow_sdk::compaction::{
    maybe_compact, CompactionConfig, CompactionOutcome,
};
use lutin_workflow_sdk::summary as sdk_summary;
use lutin_workflow_sdk::prompt::{
    AgentEntry as PromptAgentEntry, PersonaEntry as PromptPersonaEntry, PromptExtras,
};
use serde::Serialize;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
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
    /// Resolver over global + project config dirs. Shared with the
    /// runner via `RunnerCtx.resolver`; held here so request handlers
    /// (`ListPersonas`, etc.) read from the same source without
    /// re-constructing.
    resolver: Arc<Resolver>,
    events: broadcast::Sender<ChatEvent>,
    next_turn: Arc<AtomicU64>,
    /// Send-only handle to the agent runner. The runner owns the
    /// `Agent` on its task's stack — there is no shared mutable
    /// agent state in the WS layer.
    agent_cmds: mpsc::UnboundedSender<AgentCmd>,
    /// Watch receiver published by the runner on exit. Readers
    /// `.borrow()` the latest published reason without taking a lock.
    runner_failure: watch::Receiver<Option<String>>,
    /// Send-only handle to the sub-agent registry actor. Cloned into
    /// `RunnerCtx` for spawn/stop, and held here so `ListSubAgents`
    /// can serve a snapshot from the WS layer without round-tripping
    /// through the runner.
    agent_registry: mpsc::UnboundedSender<agents::AgentRegistryCmd>,
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
                    // Don't drop the connection on lag — the chrome
                    // would look "stuck" because no further events
                    // arrive. Skip the dropped events, log, and keep
                    // the subscription alive. The chrome's next
                    // explicit fetch (HistoryReplaced/MetricsReplaced
                    // ride along on turn end) reconciles state; the
                    // missed reviewer rows will reappear from the
                    // on-disk `reviews.jsonl` when the user reopens
                    // the session.
                    warn!(n, "client lagged events; skipping and continuing");
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
            let entries = store::load(&state.state_dir)
                .map_err(|e| ChatError::Internal(format!("load transcript: {e}")))?;
            Ok(ChatOk::Subscribed {
                state: s,
                history: project_history(&entries),
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
            let personas = Persona::list(&state.resolver)
                .map_err(|e| ChatError::Internal(format!("list personas: {e}")))?;
            let projected = personas
                .into_iter()
                .map(|p| principled::PersonaInfo {
                    name: p.name,
                    display_name: p.display_name,
                    model: p.model.unwrap_or_default(),
                })
                .collect();
            Ok(ChatOk::Personas { personas: projected })
        }
        ChatRequest::ListReviews => {
            let path = review::reviews_log_path(&state.state_dir);
            let raw = match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(e) => {
                    return Err(ChatError::Internal(format!(
                        "read reviews log {}: {e}",
                        path.display()
                    )));
                }
            };
            let mut reviews: Vec<principled::ReviewLogEntry> = Vec::new();
            for (lineno, line) in raw.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str(line) {
                    Ok(entry) => reviews.push(entry),
                    Err(e) => {
                        // One malformed row should not poison the whole
                        // history; skip it and keep going.
                        tracing::warn!(
                            lineno,
                            error = %e,
                            "reviews.jsonl: skipping unparseable row"
                        );
                    }
                }
            }
            Ok(ChatOk::Reviews { reviews })
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
                    .borrow()
                    .clone()
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
                    .borrow()
                    .clone()
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
            // has booted an Agent yet. Entries carry both the message
            // and its metrics in lockstep, so a single load is enough.
            let entries = store::load(&state.state_dir)
                .map_err(|e| ChatError::Internal(format!("load transcript: {e}")))?;
            Ok(ChatOk::Metrics(project_metrics(&entries)))
        }
        ChatRequest::ListSubAgents => {
            let (tx, rx) = oneshot::channel();
            if state
                .agent_registry
                .send(agents::AgentRegistryCmd::Snapshot { reply: tx })
                .is_err()
            {
                return Ok(ChatOk::SubAgents(Vec::new()));
            }
            let summaries = rx.await.unwrap_or_default();
            let projected = summaries.into_iter().map(project_summary).collect();
            Ok(ChatOk::SubAgents(projected))
        }
        ChatRequest::GetSubAgentTranscript { id } => {
            let parsed_id = id
                .parse::<agents::AgentId>()
                .map_err(|e| ChatError::Internal(e.to_string()))?;
            let (tx, rx) = oneshot::channel();
            state
                .agent_registry
                .send(agents::AgentRegistryCmd::Transcript { id: parsed_id, reply: tx })
                .map_err(|_| {
                    ChatError::Internal("sub-agent registry is unavailable".into())
                })?;
            // `Ok(None)` is "no such agent" — surface as an empty
            // transcript (the panel may have raced a `Stop`). A dropped
            // reply is a contract violation: the actor either replies
            // or its `cmd_tx` is closed (and `send` above would have
            // failed). Treat as `Internal`.
            let history = rx
                .await
                .map_err(|_| {
                    ChatError::Internal("sub-agent registry dropped reply".into())
                })?
                .map(|messages| project_messages(messages.iter()))
                .unwrap_or_default();
            Ok(ChatOk::SubAgentTranscript { id, history })
        }
    }
}

async fn mutate_via_runner(state: &AppState, op: MutateOp) -> Result<(), ChatError> {
    let (tx, rx) = oneshot::channel();
    if state.agent_cmds.send(AgentCmd::Mutate { op, reply: tx }).is_err() {
        let reason = state
            .runner_failure
            .borrow()
            .clone()
            .unwrap_or_else(|| "agent runner exited without recording a reason".into());
        return Err(ChatError::Internal(format!("agent runner unavailable: {reason}")));
    }
    rx.await
        .map_err(|_| ChatError::Internal("runner dropped mutation reply".into()))?
}

/// In-memory bookkeeping for a single turn's metrics. Created at the
/// top of `run_turn`, harvested into the corresponding `Entry` rows
/// after the turn ends.
struct TurnTracker {
    /// Wall-clock start of the turn (set at construction time, before
    /// `agent.start()`).
    started_at: Instant,
    /// First `AssistantText` delta — drives TTFT.
    first_text_at: Option<Instant>,
    /// First `AssistantReasoning` delta (if any).
    first_thinking_at: Option<Instant>,
    /// Final usage as reported by the provider on the last round.
    last_usage: Option<lutin_llm::Usage>,
    /// Cumulative prompt/completion tokens across every assistant
    /// entry committed *before* this turn started. Captured once at
    /// turn open so live `SummaryUpdated` ticks can present a
    /// monotonic running total without rescanning the transcript on
    /// each `AgentEvent::Usage`.
    total_prompt_pre_turn: u64,
    total_completion_pre_turn: u64,
    /// Cumulative tokens across every provider Usage report observed
    /// *during* this turn — one per agent-loop round. Each round is a
    /// separate API call (and a separate billable charge), so the
    /// session-wide totals must sum across rounds rather than replace
    /// with the latest round's count. Without this, a multi-round turn
    /// reports only the final round's completion in the live ticks,
    /// and `in` tracks `ctx` since both resolve to the latest round's
    /// prompt size.
    intra_turn_prompt: u64,
    intra_turn_completion: u64,
    /// Tool-call lifecycles for this turn. `Vec` rather than `HashMap`
    /// because per-turn tool counts are typically <10; linear scan is
    /// cheaper than hashing on every event.
    tools: Vec<ToolLifecycle>,
}

struct ToolLifecycle {
    call_id: String,
    started_at: Instant,
    /// Wall-clock timestamp captured when the call started; copied
    /// into `ToolStats.timestamp` at finalize time.
    started_ts: String,
    finished_at: Option<Instant>,
}

impl TurnTracker {
    /// Build a fresh tracker with the cumulative pre-turn token totals
    /// already pinned. Doing it in one constructor instead of three
    /// post-construction assignments keeps the "the tracker is built
    /// whole, then frozen except for `last_usage` / `tools`" intent
    /// readable at the call sites in `run_turn` and the rewind retry.
    fn new(pre_turn: sdk_summary::SummaryTotals) -> Self {
        Self {
            started_at: Instant::now(),
            first_text_at: None,
            first_thinking_at: None,
            last_usage: None,
            total_prompt_pre_turn: pre_turn.total_prompt_tokens,
            total_completion_pre_turn: pre_turn.total_completion_tokens,
            intra_turn_prompt: 0,
            intra_turn_completion: 0,
            tools: Vec::new(),
        }
    }
}

/// Project per-entry text-token stats into the shape consumed by
/// [`lutin_workflow_sdk::summary::aggregate`]. Lifts the
/// engine-private `Entry` shape across the SDK boundary without
/// dragging the full `MessageMetrics` into the shared crate.
fn entry_tokens(entries: &[Entry]) -> impl Iterator<Item = sdk_summary::EntryTokens> + '_ {
    entries.iter().map(|e| sdk_summary::EntryTokens {
        prompt_tokens: e.metrics.text.and_then(|t| t.prompt_tokens),
        completion_tokens: e.metrics.text.and_then(|t| t.completion_tokens),
    })
}

/// Build a `SummaryUpdated` payload from the committed transcript.
/// Aggregation logic is shared with the chat engine via
/// `lutin-workflow-sdk`; only the wire-event wrap is workflow-local.
fn build_summary_updated(entries: &[Entry]) -> ChatEvent {
    let s = sdk_summary::aggregate(entry_tokens(entries));
    ChatEvent::SummaryUpdated {
        context_tokens: s.context_tokens,
        total_prompt_tokens: s.total_prompt_tokens,
        total_completion_tokens: s.total_completion_tokens,
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
    project_config_dir: PathBuf,
    /// Resolver over the global + project config dirs. Built once at
    /// startup and shared via `Arc` so per-call clones don't fan out
    /// `PathBuf`s. Anything reading personas / settings goes through
    /// here rather than re-constructing.
    resolver: Arc<Resolver>,
    events: broadcast::Sender<ChatEvent>,
    /// Shared with `AppState`. Runner writes the failure reason here
    /// on exit; readers consult the latest published value via the
    /// watch channel without taking a lock.
    failure: watch::Sender<Option<String>>,
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
        // First-write wins — later transient errors after the initial
        // failure shouldn't overwrite the root cause. `send_if_modified`
        // gives us read-then-write atomicity without a lock.
        self.failure.send_if_modified(|slot| {
            if slot.is_none() {
                *slot = Some(reason);
                true
            } else {
                false
            }
        });
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
    let mut entries: Vec<Entry> = match store::load(&ctx.state_dir) {
        Ok(es) => es,
        Err(e) => {
            ctx.record_failure(format!("load transcript: {e}"));
            return;
        }
    };
    // Refresh summary.json on boot so a resumed session gets its
    // last_activity bumped (and a freshly-created session gets a file
    // at all, even before the first turn).
    write_summary(&ctx, &entries);

    let mut agent: Option<Agent> = None;
    // Review session lives in the runner task — the only writer of
    // its step stack and its bundle list. `ApprovalPolicy::decide`
    // talks to it via a channel; `apply_mutation` reads it directly
    // via `&mut`. Reset to `None` between turns; built fresh in
    // `run_turn` when the session has principles configured.
    let mut review_session: Option<review::ReviewSession> = None;
    loop {
        tokio::select! {
            cmd = rx.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    AgentCmd::Send { text, turn } => {
                        if let Some(a) = ensure_agent(&mut agent, &ctx, &entries, turn) {
                            run_turn(
                                &ctx, &mut rx, a, &mut entries, &mut review_session,
                                Some(text), turn,
                            ).await;
                        }
                    }
                    AgentCmd::Rerun { turn } => {
                        if let Some(a) = ensure_agent(&mut agent, &ctx, &entries, turn) {
                            run_turn(
                                &ctx, &mut rx, a, &mut entries, &mut review_session,
                                None, turn,
                            ).await;
                        }
                    }
                    AgentCmd::Cancel => {} // idle — nothing to cancel
                    AgentCmd::Mutate { op, reply } => {
                        let active = review_session
                            .as_ref()
                            .is_some_and(|s| s.has_active_frame());
                        if active {
                            let _ = reply.send(Err(ChatError::ReviewInFlight));
                        } else {
                            let result = apply_mutation(&ctx, agent.as_mut(), &mut entries, op);
                            let _ = reply.send(result);
                        }
                    }
                }
            }
            evt = completions_rx.recv() => match evt {
                Some(evt) => {
                    handle_subagent_completion(
                        &ctx, &mut rx, &mut agent, &mut entries, &mut review_session, evt,
                    ).await;
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
    // Mirror the seed user prompt into the slot before the run starts —
    // the SDK's `start()` consumes it but doesn't replay it through the
    // event stream, so without this push the read-only UI panel would
    // open with no first turn visible. Clone here because `spec` moves
    // into `build_subagent` next.
    let _ = update_tx.send(agents::AgentUpdate::TranscriptAppend {
        id,
        message: lutin_llm::Message::User(spec.initial_prompt.clone()),
    });
    let mut agent = match build_subagent(&ctx, spec, id) {
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
                let _ = update_tx.send(agents::AgentUpdate::Progress {
                    id,
                    last_text: s,
                });
            }
            AgentEvent::AssistantMessage(msg) => {
                // Final assistant turn for this round (text + tool_calls
                // + thinking, all in one variant). Push as-is so the UI
                // mirrors the SDK's transcript exactly.
                let _ = update_tx.send(agents::AgentUpdate::TranscriptAppend {
                    id,
                    message: msg,
                });
            }
            AgentEvent::ToolCallCompleted { call, outcome } => {
                // Synthesize a `ToolResult` message so the read-only
                // panel shows what the tool returned alongside the
                // assistant turn that requested it. The SDK appends
                // the same shape internally on its way back into the
                // next round.
                let content = match outcome {
                    ToolResult::Ok(c) => c,
                    ToolResult::Err(e) => lutin_llm::ToolResultContent {
                        call_id: call.id.clone(),
                        content: format!("{e}"),
                        is_error: true,
                    },
                    // The enum is `#[non_exhaustive]`; treat any future
                    // variant as an opaque error so the panel still
                    // shows something instead of silently dropping the
                    // round.
                    other => lutin_llm::ToolResultContent {
                        call_id: call.id.clone(),
                        content: format!("{other:?}"),
                        is_error: true,
                    },
                };
                let _ = update_tx.send(agents::AgentUpdate::TranscriptAppend {
                    id,
                    message: lutin_llm::Message::ToolResult(content),
                });
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
            // Reuse the chat-protocol mapping so the wording stays in
            // lockstep with `run_turn`'s terminal events.
            let error = match map_finish_reason(other) {
                FinishReason::Failed(reason) => reason,
                FinishReason::Cancelled => "cancelled".into(),
                FinishReason::Completed => "completed (unreachable)".into(),
                FinishReason::MaxRounds => "max rounds (unreachable)".into(),
            };
            let _ = update_tx.send(agents::AgentUpdate::Failed { id, error });
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
/// Derive `PromptExtras` for one chat turn so the SDK can substitute
/// `%message_count%`, `%user_message%`, `%agents:attached%`, etc. in
/// the persona's system prompt. Pulls live data from the on-disk
/// transcript and the sub-agent registry, plus a project-then-global
/// persona listing for `%personas:all%`. Field-by-field — none are
/// load-bearing, so a registry that won't snapshot or a resolver
/// that returns no personas just leaves those placeholders empty.
async fn build_prompt_extras(
    ctx: &RunnerCtx,
    entries: &[Entry],
    current_persona: &Persona,
) -> PromptExtras {
    let user_message = entries.iter().rev().find_map(|e| match &e.message {
        lutin_llm::Message::User(t) => Some(t.clone()),
        _ => None,
    });
    let latest_response = entries.iter().rev().find_map(|e| match &e.message {
        lutin_llm::Message::Assistant { text, .. } if !text.is_empty() => Some(text.clone()),
        _ => None,
    });

    // Snapshot the registry; ignore any failure — empty list is the
    // right fallback (a vanished registry == no children, from the
    // LLM's perspective).
    let attached_agents: Vec<PromptAgentEntry> = fetch_summaries(ctx)
        .await
        .into_iter()
        .map(|sum| PromptAgentEntry {
            name: sum.id.to_string(),
            status: status_label(&sum.status).to_owned(),
        })
        .collect();

    // Persona list for `%personas:all%`. Exclude the current persona
    // so the LLM doesn't see itself as a delegation target.
    let personas = Persona::list(&ctx.resolver)
        .map(|all| {
            all.into_iter()
                .filter(|p| p.name != current_persona.name)
                .map(|p| PromptPersonaEntry {
                    name: p.name,
                    display_name: p.display_name,
                    description: p.description,
                })
                .collect()
        })
        .unwrap_or_default();

    PromptExtras {
        message_count: entries.len(),
        user_message,
        latest_response,
        attached_agents,
        personas,
        chat_kind: "main".into(),
        ..PromptExtras::default()
    }
}

/// One round-trip to the registry actor. Returns an empty vec when
/// the cmd channel is closed or the reply is dropped — both mean "no
/// children visible" from the caller's POV. Extracted so `subagent_block`,
/// `build_prompt_extras`, `broadcast_subagents`, and `ListSubAgents`
/// don't open-code the same dance.
async fn fetch_summaries(ctx: &RunnerCtx) -> Vec<agents::AgentSummary> {
    let (tx, rx) = oneshot::channel();
    if ctx
        .agent_registry
        .send(agents::AgentRegistryCmd::Snapshot { reply: tx })
        .is_err()
    {
        return Vec::new();
    }
    rx.await.unwrap_or_default()
}

/// Project an `AgentSummary` to the chat protocol's `SubAgentInfo` wire
/// shape. Pure — caller wraps in `Vec` if it's snapshotting many.
fn project_summary(s: agents::AgentSummary) -> SubAgentInfo {
    SubAgentInfo {
        id: s.id.to_string(),
        parent_id: s.parent_id.map(|p| p.to_string()),
        persona: s.persona,
        status: match s.status {
            agents::AgentStatus::Running => SubAgentStatus::Running,
            agents::AgentStatus::Completed => SubAgentStatus::Completed,
            agents::AgentStatus::Failed { reason } => SubAgentStatus::Failed { reason },
            agents::AgentStatus::Stopped => SubAgentStatus::Stopped,
        },
        last_progress: s.last_progress,
    }
}

/// Snapshot one child's transcript and emit
/// `SubAgentTranscriptUpdated`. Empty when the registry is gone or
/// the id is unknown — both indistinguishable from the UI's POV. A
/// dropped oneshot reply is logged as a contract violation rather
/// than collapsed into the same empty case.
async fn broadcast_subagent_transcript(ctx: &RunnerCtx, id: agents::AgentId) {
    let (tx, rx) = oneshot::channel();
    if ctx
        .agent_registry
        .send(agents::AgentRegistryCmd::Transcript { id, reply: tx })
        .is_err()
    {
        return;
    }
    let messages = match rx.await {
        Ok(opt) => opt.unwrap_or_default(),
        Err(_) => {
            warn!(%id, "registry dropped Transcript reply");
            return;
        }
    };
    let history = project_messages(messages.iter());
    let _ = ctx.events.send(ChatEvent::SubAgentTranscriptUpdated {
        id: id.to_string(),
        history,
    });
}

/// Snapshot+broadcast helper: emit `SubAgentsChanged` on the engine's
/// event channel. Empty payloads are informative (a final terminal
/// transition can leave the list empty), so we always send.
async fn broadcast_subagents(ctx: &RunnerCtx) {
    let snap = fetch_summaries(ctx).await.into_iter().map(project_summary).collect();
    let _ = ctx.events.send(ChatEvent::SubAgentsChanged(snap));
}

/// Render the `<active_subagents>` block injected into the
/// orchestrator's system prompt. `None` when the registry is empty or
/// unreachable; both mean "no block to inject" — the LLM can't tell
/// the difference and shouldn't.
async fn subagent_block(ctx: &RunnerCtx) -> Option<String> {
    let summaries = fetch_summaries(ctx).await;
    if summaries.is_empty() {
        return None;
    }
    let mut out = String::from("<active_subagents>\n");
    for s in &summaries {
        out.push_str("- ");
        out.push_str(&s.id.to_string());
        out.push_str(" status=");
        out.push_str(status_label(&s.status));
        if let agents::AgentStatus::Failed { reason } = &s.status {
            out.push_str(&format!(" reason={reason:?}"));
        }
        if let Some(p) = &s.last_progress {
            out.push_str(&format!(" progress={p:?}"));
        }
        out.push('\n');
    }
    out.push_str("</active_subagents>");
    Some(out)
}

/// Stable lowercase label for an `AgentStatus` — one place where the
/// "running / completed / failed / stopped" wording lives, shared by
/// the system-prompt block and the prompt-extras `attached_agents`
/// projection.
fn status_label(status: &agents::AgentStatus) -> &'static str {
    match status {
        agents::AgentStatus::Running => "running",
        agents::AgentStatus::Completed => "completed",
        agents::AgentStatus::Failed { .. } => "failed",
        agents::AgentStatus::Stopped => "stopped",
    }
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
    entries: &mut Vec<Entry>,
    review_session: &mut Option<review::ReviewSession>,
    evt: agents::CompletionEvent,
) {
    // Transcript appends are not turn-triggering events — the child is
    // still mid-run, no parent-side message to inject yet. Just relay
    // the latest transcript so any open child view streams in real time.
    if let agents::CompletionEvent::TranscriptAppend { id, .. } = &evt {
        broadcast_subagent_transcript(ctx, *id).await;
        return;
    }
    let turn = ctx.next_turn();
    // Push a fresh sub-agent snapshot first so the UI panel reflects
    // the terminal transition even when the parent has no agent (build
    // failure path bails below without touching the registry).
    broadcast_subagents(ctx).await;
    // ensure_agent fabricates a `MessageFinished{Failed}` on its own
    // when the build fails, so any UI watching the auto-turn sees a
    // terminal event even if we bail before run_turn.
    let Some(a) = ensure_agent(agent, ctx, entries, turn) else {
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
        // Already returned above.
        agents::CompletionEvent::TranscriptAppend { .. } => unreachable!(),
    };
    if let Err(e) = a.push_message(msg.clone()) {
        warn!(error = %e, "push agent response failed; skipping auto-turn");
        return;
    }
    // Mirror the agent's append into our entries vec, stamping with the
    // moment the parent saw the response (not when it was generated
    // upstream).
    entries.push(Entry {
        message: msg,
        metrics: MessageMetrics {
            timestamp: Some(now_rfc3339()),
            ..Default::default()
        },
    });
    // Persist + broadcast before the turn streams so subscribers see
    // the new entry alongside (or before) any assistant deltas. The
    // turn's tail does its own save — this earlier write is the cost
    // of giving the chrome a HistoryReplaced anchor for the injected
    // message.
    if let Err(e) = store::save(&ctx.state_dir, entries) {
        warn!(error = %e, "save transcript after agent response failed");
    }
    write_summary(ctx, entries);
    let _ = ctx.events.send(ChatEvent::HistoryReplaced(project_history(entries)));
    let _ = ctx.events.send(ChatEvent::MetricsReplaced(project_metrics(entries)));
    let _ = ctx.events.send(build_summary_updated(entries));
    // text=None: the agent-response message is already on the
    // transcript; we just want the agent loop to take a turn against it.
    run_turn(ctx, rx, a, entries, review_session, None, turn).await;
}

/// Apply one mutation op to the canonical history. Mutates the in-memory
/// `entries` vec, then mirrors the new message list into the agent (if
/// one exists) and persists. Each `Entry` carries its own metrics, so
/// mutations move data and metrics together — there's no parallel-vec
/// realignment step.
fn apply_mutation(
    ctx: &RunnerCtx,
    agent: Option<&mut Agent>,
    entries: &mut Vec<Entry>,
    op: MutateOp,
) -> Result<(), ChatError> {
    // Note: the `ReviewInFlight` guard is upstream in
    // `run_agent_loop` so this function doesn't need to read review
    // state. The runner is the single writer of that state and
    // checks it before dispatching here.
    if let Some(a) = agent {
        // Reject the mutation up-front when a turn is streaming. The
        // SDK's edit_messages will also reject, but its error is opaque
        // — checking here lets the UI surface `TurnInFlight` cleanly.
        let mut applied: Result<(), ChatError> = Ok(());
        a.edit_messages(|_| {
            applied = mutate_entries(entries, &op);
        })
        .map_err(|_| ChatError::TurnInFlight)?;
        applied?;
        // Now sync the mutated message list back into the agent.
        let msgs = store::messages(entries);
        a.edit_messages(|m| *m = msgs)
            .map_err(|_| ChatError::TurnInFlight)?;
    } else {
        mutate_entries(entries, &op)?;
    }
    // Mutation already mutated in-memory state; if disk persistence
    // fails the user needs to know — silently warning would leave the
    // chat acknowledged-as-applied while the next session-restart
    // would resurrect the pre-mutation transcript.
    store::save(&ctx.state_dir, entries).map_err(|e| {
        warn!(error = %e, "save transcript after mutation failed");
        ChatError::PersistFailed { op: "save transcript".into() }
    })?;
    write_summary(ctx, entries);
    let _ = ctx.events.send(ChatEvent::HistoryReplaced(project_history(entries)));
    let _ = ctx.events.send(ChatEvent::MetricsReplaced(project_metrics(entries)));
    let _ = ctx.events.send(build_summary_updated(entries));
    Ok(())
}

/// Apply `op` to `entries`. Edit/Delete/DeleteFrom find the target via
/// the projected-index → entry-index mapping, then operate on the entry
/// in place. Tool-call slot edits are rejected as out-of-range so the
/// UI can disable the menu.
fn mutate_entries(entries: &mut Vec<Entry>, op: &MutateOp) -> Result<(), ChatError> {
    use lutin_llm::Message;
    let (entry_idx, slot) = locate_entry(entries, projected_index(op))?;
    match op {
        MutateOp::Edit { text, .. } => match (&mut entries[entry_idx].message, slot) {
            (Message::User(t), ProjectedSlot::User) => *t = text.clone(),
            (Message::Assistant { thinking, .. }, ProjectedSlot::Thinking) => {
                *thinking = Some(text.clone());
            }
            (Message::Assistant { text: at, .. }, ProjectedSlot::AssistantText) => {
                *at = text.clone();
            }
            (_, ProjectedSlot::Tool | ProjectedSlot::SubAgent) => {
                return Err(ChatError::HistoryIndexOutOfRange(projected_index(op)));
            }
            _ => unreachable!("slot resolved against same entries"),
        },
        MutateOp::Delete { .. } => match slot {
            ProjectedSlot::User => {
                entries.remove(entry_idx);
            }
            ProjectedSlot::Thinking => {
                if let Message::Assistant { thinking, .. } = &mut entries[entry_idx].message {
                    *thinking = None;
                }
                entries[entry_idx].metrics.thinking = None;
            }
            ProjectedSlot::AssistantText => {
                if let Message::Assistant { text, .. } = &mut entries[entry_idx].message {
                    text.clear();
                }
                entries[entry_idx].metrics.text = None;
            }
            ProjectedSlot::Tool | ProjectedSlot::SubAgent => {
                return Err(ChatError::HistoryIndexOutOfRange(projected_index(op)));
            }
        },
        MutateOp::DeleteFrom { .. } => {
            entries.truncate(entry_idx);
        }
    }
    Ok(())
}

fn projected_index(op: &MutateOp) -> u32 {
    match op {
        MutateOp::Edit { index, .. }
        | MutateOp::Delete { index }
        | MutateOp::DeleteFrom { index } => *index,
    }
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

/// Walk `entries` in projected order, yielding `(entry_index, slot)`
/// for each visible row. Mirrors `project_history` exactly so projected
/// indices line up between the wire and the runner.
fn projected_slots(entries: &[Entry]) -> impl Iterator<Item = (usize, ProjectedSlot)> + '_ {
    use lutin_llm::Message;
    entries.iter().enumerate().flat_map(|(i, e)| {
        let user = matches!(&e.message, Message::User(t) if !t.is_empty())
            .then_some(ProjectedSlot::User);
        let sub_agent = matches!(
            &e.message,
            Message::SubAgentReply { .. } | Message::SubAgentFailure { .. }
        )
        .then_some(ProjectedSlot::SubAgent);
        let (thinking, text, tools_count) = match &e.message {
            Message::Assistant { text, thinking, tool_calls } => (
                thinking
                    .as_deref()
                    .is_some_and(|s| !s.is_empty())
                    .then_some(ProjectedSlot::Thinking),
                (!text.is_empty()).then_some(ProjectedSlot::AssistantText),
                tool_calls.len(),
            ),
            _ => (None, None, 0),
        };
        user.into_iter()
            .chain(sub_agent)
            .chain(thinking)
            .chain(text)
            .chain(std::iter::repeat(ProjectedSlot::Tool).take(tools_count))
            .map(move |s| (i, s))
    })
}

/// Resolve a projected index to the underlying `(entry_index, slot)`,
/// or report it out of range.
fn locate_entry(
    entries: &[Entry],
    index: u32,
) -> Result<(usize, ProjectedSlot), ChatError> {
    projected_slots(entries)
        .nth(index as usize)
        .ok_or(ChatError::HistoryIndexOutOfRange(index))
}

/// Lazy-build the agent on first use; surface init failures as a
/// turn-level error so the runner stays alive. Returns `None` when
/// the build failed (and the caller should skip the turn).
fn ensure_agent<'a>(
    slot: &'a mut Option<Agent>,
    ctx: &RunnerCtx,
    entries: &[Entry],
    turn: TurnId,
) -> Option<&'a mut Agent> {
    if slot.is_none() {
        match build_initial_agent(ctx, entries) {
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

/// Build the agent on first use, seeding it from the in-memory entries
/// (which were loaded once at runner start and stay authoritative).
fn build_initial_agent(ctx: &RunnerCtx, entries: &[Entry]) -> Result<Agent, String> {
    let resolved = resolve_args(ctx, None).map_err(|e| format!("resolve args: {e}"))?;
    let mut agent = sdk_build_agent(resolved.as_build_args(ctx))
        .map_err(|e| format!("build agent: {}", map_build_error(e)))?;
    let messages = store::messages(entries);
    agent
        .edit_messages(|m| *m = messages)
        .map_err(|e| format!("seed agent messages: {e}"))?;
    Ok(agent)
}

/// Build a sub-agent from an [`agents::AgentSpec`]. The persona inside
/// the spec was already resolved at the `SpawnAgent` tool boundary, so
/// there's no second persona load here — only `Settings` + sandbox
/// derivation. The initial user prompt is queued so the caller's
/// `agent.start()` consumes it on the first round.
///
/// Returns owned errors (not `ChatError`) — sub-agent failures surface
/// to the registry as `AgentUpdate::Failed { error }`, not to the chat
/// protocol layer.
fn build_subagent(
    ctx: &RunnerCtx,
    spec: agents::AgentSpec,
    owner_id: agents::AgentId,
) -> Result<Agent, String> {
    let agents::AgentSpec { initial_prompt, persona, parent_id: _ } = spec;
    let resolved = resolve_args(ctx, Some(persona))
        .map_err(|e| format!("resolve args: {e}"))?;
    let build_args =
        resolved.as_build_args_with(ctx, PromptExtras::default(), Some(owner_id));
    let mut agent = sdk_build_agent(build_args)
        .map_err(|e| format!("build agent: {}", map_build_error(e)))?;
    agent
        .push_message(lutin_llm::Message::User(initial_prompt))
        .map_err(|e| format!("push initial prompt: {e}"))?;
    Ok(agent)
}

/// `text` is `Some(_)` for a new user message and `None` for a Rerun,
/// which kicks the agent loop against the existing transcript.
/// Outcome of the rewind channel for one turn iteration. `Continue`
/// restores the snapshot and restarts the agent (the existing rethink
/// path); `Abort` restores the snapshot and halts the turn with a
/// `Failed` reason — used when the review system itself can't make a
/// decision and auto-retrying would just amplify load against a
/// wedged backend.
#[derive(Debug, Clone)]
enum PendingRewind {
    Continue { feedback: String },
    Abort { reason: String },
}

async fn run_turn(
    ctx: &RunnerCtx,
    rx: &mut mpsc::UnboundedReceiver<AgentCmd>,
    agent: &mut Agent,
    entries: &mut Vec<Entry>,
    review_session: &mut Option<review::ReviewSession>,
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
    let extras = build_prompt_extras(ctx, entries, &resolved.persona).await;
    if let Err(e) = sdk_refresh_agent(agent, resolved.as_build_args_with(ctx, extras, None)) {
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
    // Compile-time `principles.toml` declares the workflow's principle
    // order (least-important first). At runtime we intersect that with
    // what's actually installed on disk: missing files are warned and
    // dropped, installed-but-unlisted files are ignored. The result is
    // the per-turn principle list — reloaded on every turn so
    // out-of-band edits to `<config>/principles/` (add, tweak
    // `applies_to`) take effect on the next send. A truly broken
    // principle file will surface again when `review::build` tries to
    // load it by name.
    let principle_names: Vec<String> = match crate::principle::Principle::list(&ctx.resolver) {
        Ok(installed) => {
            let installed: std::collections::HashSet<&str> =
                installed.iter().map(|p| p.name.as_str()).collect();
            let mut out = Vec::with_capacity(crate::principle::WORKFLOW_ORDER.len());
            for name in crate::principle::WORKFLOW_ORDER.iter() {
                if installed.contains(name) {
                    out.push((*name).to_string());
                } else {
                    tracing::warn!(
                        principle = name,
                        "principles.toml lists a principle that isn't installed; skipping"
                    );
                }
            }
            out
        }
        Err(e) => {
            tracing::warn!(error = %e, "Principle::list failed; turn runs ungated");
            Vec::new()
        }
    };
    // Channels for this turn:
    //  - rewind_rx:  Rethink verdict → cancel + restore prior frame.
    //  - review_req_rx: BeginFrame / ApplyVerdicts requests from
    //    `ApprovalPolicy::decide`. The session is the single writer of
    //    its step stack — `decide` only sends requests and awaits
    //    replies on per-call oneshots.
    let (rewind_tx, mut rewind_rx) = mpsc::unbounded_channel::<review::RewindSignal>();
    let (review_req_tx, mut review_req_rx) =
        mpsc::unbounded_channel::<review::ReviewRequest>();
    if !principle_names.is_empty() {
        // Install the review policy. Failure here means the user's
        // configured principles wouldn't gate tool calls — the turn
        // would silently run unreviewed, which is a correctness bug,
        // not a degrade-and-warn. Bail.
        let install = (|| -> Result<(), String> {
            let (policy, session) = review::build(
                &ctx.resolver,
                &resolved.settings,
                &principle_names,
                review_req_tx.clone(),
                rewind_tx,
                ctx.events.clone(),
                ctx.state_dir.clone(),
                resolved.review_concurrency,
            )
            .map_err(|e| format!("build review policy: {e}"))?;
            // Persist the step stack across turns, but refresh the
            // bundle list (provider/model can change between turns
            // when persona or settings are edited live).
            match review_session.as_mut() {
                Some(existing) => {
                    existing.bundles = session.bundles;
                    existing.rewind_tx = session.rewind_tx;
                }
                None => *review_session = Some(session),
            }
            agent
                .try_set_approval(Box::new(policy))
                .map_err(|e| format!("install review policy: {e}"))?;
            // Cap the agent at one tool call per round. Only the first
            // call's outcome shapes what comes next anyway, and
            // reviewing N calls in lockstep would conflate independent
            // decisions.
            agent
                .update_config(|cfg| cfg.tool_policy.max_calls_per_round = 1)
                .map_err(|e| format!("set max_calls_per_round: {e}"))?;
            Ok(())
        })();
        if let Err(reason) = install {
            let _ = ctx.events.send(ChatEvent::MessageFinished {
                turn_id: turn,
                reason: FinishReason::Failed(reason),
            });
            return;
        }
    } else {
        // No principles configured: drop any session left over from a
        // prior turn so the apply_mutation guard reads clean.
        *review_session = None;
    }
    // Suppress the unused-warning for `review_req_tx`; the policy now
    // owns the only clone that matters. The local binding is kept so
    // the channel pair is constructed in one place.
    drop(review_req_tx);
    // Pre-turn compaction: when the persona opts in and the transcript
    // has crossed the threshold, fold the older prefix into a single
    // `Message::Summary` and archive the originals to a sidecar so the
    // user can still inspect what was dropped.
    run_compaction(ctx, agent, &resolved, entries).await;
    // start() consumes pending messages on this run, so push first
    // (skipped on Rerun, which deliberately runs against the existing
    // transcript without appending a new user message).
    if let Some(text) = text {
        let user_msg = lutin_llm::Message::User(text);
        if let Err(e) = agent.push_message(user_msg.clone()) {
            let _ = ctx.events.send(ChatEvent::MessageFinished {
                turn_id: turn,
                reason: FinishReason::Failed(format!("push: {e}")),
            });
            return;
        }
        // Mirror into entries with a timestamp so the chrome can render
        // the user's bubble (with timestamp) before the assistant
        // starts streaming.
        entries.push(Entry {
            message: user_msg,
            metrics: MessageMetrics {
                timestamp: Some(now_rfc3339()),
                ..Default::default()
            },
        });
        if let Err(e) = store::save(&ctx.state_dir, entries) {
            warn!(error = %e, "save transcript after user push failed");
        }
        let _ = ctx.events.send(ChatEvent::HistoryReplaced(project_history(entries)));
        let _ = ctx.events.send(ChatEvent::MetricsReplaced(project_metrics(entries)));
        let _ = ctx.events.send(build_summary_updated(entries));
    }
    let pre_turn_len = entries.len();
    let mut tracker = TurnTracker::new(sdk_summary::aggregate(entry_tokens(entries)));
    // Live transcript length tracked from agent events. The runner is
    // the only writer; `decide` reads it through the
    // `BeginFrame.live_messages_len` parameter that the runner
    // forwards when servicing the request.
    let mut live_messages_len: usize = pre_turn_len;
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
    // Outer rewind loop: each iteration runs one full agent round-loop
    // against the (possibly truncated) transcript. The inner round
    // loop drives the SDK event stream; if a reviewer signals rewind
    // via `rewind_rx`, we capture the feedback, cancel the agent, let
    // the stream drain so `agent.join()` returns cleanly, then
    // `perform_rewind` pops the failed step (and its file effects)
    // and the loop spins to start a fresh `agent.start()` against the
    // rewound transcript. Bottom-of-stack ends the turn with a Failed
    // reason.
    let mut pending_feedback: Option<PendingRewind> = None;
    let outcome = 'rewind: loop {
        'round: loop {
            tokio::select! {
                ev = stream.next() => match ev {
                    Some(ev) => {
                        update_live_messages_len(&mut live_messages_len, &ev);
                        if let Some(reason) = handle_agent_event(ev, &ctx.events, &mut tracker) {
                            finish = Some(reason);
                        }
                    }
                    None => break 'round,
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
                },
                Some(req) = review_req_rx.recv() => {
                    if let Some(s) = review_session.as_mut() {
                        s.handle(req, live_messages_len);
                    } else {
                        debug_assert!(false, "review request without an active session");
                    }
                }
                Some(signal) = rewind_rx.recv() => {
                    // Coalesce: if multiple signals fire before we can
                    // cancel, the most recent one wins. Abort
                    // outranks Continue — once the review system has
                    // failed, restarting the agent is pointless.
                    match signal {
                        review::RewindSignal::Continue { feedback } => {
                            if !matches!(pending_feedback, Some(PendingRewind::Abort { .. })) {
                                pending_feedback = Some(PendingRewind::Continue { feedback });
                            }
                        }
                        review::RewindSignal::Abort { reason } => {
                            pending_feedback = Some(PendingRewind::Abort { reason });
                        }
                    }
                    agent.cancel();
                    // Keep draining the stream until it ends so the SDK
                    // task observes the cancel and join completes
                    // cleanly.
                }
            }
        }
        let outcome = agent.join().await;
        let Some(pending) = pending_feedback.take() else {
            break 'rewind outcome;
        };
        // Both branches restore file snapshots first; the difference
        // is what happens after — Continue restarts the agent against
        // the rewound transcript, Abort halts the turn with a Failed
        // reason and surfaces the error to the user.
        let (label, restart_after_rewind) = match &pending {
            PendingRewind::Continue { feedback } => (feedback.as_str(), true),
            PendingRewind::Abort { reason } => (reason.as_str(), false),
        };
        match perform_rewind(
            agent,
            entries,
            review_session.as_mut(),
            &mut live_messages_len,
            &ctx.events,
            label,
        ) {
            Ok(true) if restart_after_rewind => {
                // Successful rewind: reset tracker for the new round
                // so finalize_turn_meta gets clean stats for the
                // post-rewind portion.
                tracker = TurnTracker::new(sdk_summary::aggregate(entry_tokens(entries)));
                stream = match agent.start() {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = ctx.events.send(ChatEvent::MessageFinished {
                            turn_id: turn,
                            reason: FinishReason::Failed(format!("restart after rewind: {e}")),
                        });
                        return;
                    }
                };
            }
            Ok(_) => {
                // Either an Abort (review system failure: do not
                // restart, surface the error) or a Continue with
                // nothing to rewind to (no prior step) → both end
                // the turn with Failed.
                finish = Some(FinishReason::Failed(match pending {
                    PendingRewind::Abort { reason } => {
                        format!("review system failure: {reason}")
                    }
                    PendingRewind::Continue { feedback } => {
                        format!("review escalated (no prior step to rewind to): {feedback}")
                    }
                }));
                break 'rewind outcome;
            }
            Err(e) => {
                warn!(error = %e, "perform_rewind failed; ending turn");
                finish = Some(FinishReason::Failed(format!("rewind failed: {e}")));
                break 'rewind outcome;
            }
        }
    };
    // Squash rejected attempts in the agent's transcript before
    // mirroring into entries: a step that ultimately accepts shouldn't
    // leave its denied predecessors lingering in the model's context.
    // No-op for sessions without principles (no `denied:` results show
    // up there).
    if let Err(e) = agent.edit_messages(|m| squash_denied_attempts(m, pre_turn_len)) {
        warn!(error = %e, "squash denied attempts failed (agent busy)");
    }
    // Sync any messages the agent appended into our entries vec, then
    // attribute the turn's accumulated stats. The last `Message::Assistant`
    // gets the full set (TTFT, duration, tokens); intermediates get
    // just a timestamp; tool calls pull from the tracker by call_id.
    sync_new_entries(agent.messages(), entries);
    finalize_turn_meta(entries, pre_turn_len, &tracker);
    if let Err(e) = store::save(&ctx.state_dir, entries) {
        warn!(error = %e, "save transcript failed");
    }
    write_summary(ctx, entries);
    let reason = finish.unwrap_or_else(|| map_finish_reason(outcome.finish_reason));
    let _ = ctx.events.send(ChatEvent::MessageFinished {
        turn_id: turn,
        reason,
    });
    let _ = ctx.events.send(ChatEvent::HistoryReplaced(project_history(entries)));
    let _ = ctx.events.send(ChatEvent::MetricsReplaced(project_metrics(entries)));
    let _ = ctx.events.send(build_summary_updated(entries));
    // Capture any spawn / stop / terminal transition the orchestrator
    // produced during this turn. Snapshotting once at the tail rather
    // than per-tool-call avoids spamming the channel; the only cost is
    // intra-turn UI lag, which is fine for an audit panel.
    broadcast_subagents(ctx).await;
}

/// Track the agent's live transcript length from the event stream.
/// `agent.messages()` is empty during a run (the buffer is moved into
/// the run task), so the runner re-derives the count from
/// `AssistantMessage` and `ToolCallCompleted` events. The runner is
/// the only writer; the count is forwarded into `ReviewSession` when
/// servicing a `BeginFrame` request so the new frame's snapshot index
/// matches the row a future rewind needs to truncate to.
fn update_live_messages_len(len: &mut usize, ev: &AgentEvent) {
    let delta = match ev {
        AgentEvent::AssistantMessage(_) => 1,
        AgentEvent::ToolCallCompleted { .. } => 1,
        _ => return,
    };
    *len = len.saturating_add(delta);
}

/// Pop the top frame, restore both file snapshots, truncate the
/// agent's messages and our `entries` to the prior frame's
/// conversation_index, and append a synthetic user-message that hands
/// the carried_forward feedback to the agent. Returns `Ok(true)` when
/// the rewind succeeded and a new `agent.start()` should run; returns
/// `Ok(false)` when the failed step was the bottom of the stack
/// (caller should surface to user). Any IO / state error becomes
/// `Err`.
fn perform_rewind(
    agent: &mut Agent,
    entries: &mut Vec<Entry>,
    session: Option<&mut review::ReviewSession>,
    live_messages_len: &mut usize,
    events: &broadcast::Sender<ChatEvent>,
    feedback: &str,
) -> Result<bool, String> {
    let session = session.ok_or_else(|| "rewind requested with no active session".to_string())?;
    let outcome = session
        .stack
        .rewind(feedback)
        .map_err(|e| format!("rewind file restore: {e}"))?;
    let truncate_to = match outcome {
        crate::step::RewindOutcome::Rewound { reactivated } => session
            .stack
            .frames()
            .iter()
            .find(|f| f.id == reactivated)
            .map(|f| f.snapshot.conversation_index)
            .ok_or_else(|| "reactivated frame not found".to_string())?,
        crate::step::RewindOutcome::BottomOfStack => return Ok(false),
    };

    // Truncate the agent's transcript and our own entries vector to
    // the moment the prior frame's tool would have run. The synthetic
    // user-prompt below tells the agent to reconsider that step.
    if let Err(e) = agent.edit_messages(|m| {
        if truncate_to <= m.len() {
            m.truncate(truncate_to);
        }
    }) {
        return Err(format!("agent.edit_messages: {e}"));
    }
    if truncate_to <= entries.len() {
        entries.truncate(truncate_to);
    }

    let synthetic = format!(
        "[review rewind] An earlier step was rolled back. Reconsider the most recent step \
         from a different angle. Reviewer feedback: {feedback}"
    );
    let user_msg = lutin_llm::Message::User(synthetic);
    if let Err(e) = agent.push_message(user_msg.clone()) {
        return Err(format!("agent.push_message: {e}"));
    }
    entries.push(Entry {
        message: user_msg,
        metrics: MessageMetrics {
            timestamp: Some(now_rfc3339()),
            ..Default::default()
        },
    });

    // Update live transcript length to match the truncated +
    // synthetic state so the next frame's snapshot index is correct.
    *live_messages_len = entries.len();

    let _ = events.send(ChatEvent::HistoryReplaced(project_history(entries)));
    let _ = events.send(ChatEvent::MetricsReplaced(project_metrics(entries)));
    let _ = events.send(build_summary_updated(entries));
    Ok(true)
}

/// Append a fresh `Entry` for every message the agent added past
/// `entries.len()`. Each new entry gets a `now()` timestamp; the
/// per-stat fields are filled in by `finalize_turn_meta`.
/// Drop rejected review attempts from the agent's transcript so the
/// final state shows only the accepted step's tool_use → tool_result.
/// Detection delegated to `review::is_review_denial` — that's the
/// shared contract with the policy that emits the denial. A genuine
/// in-tool error or a deny from some other future approval source
/// won't match.
///
/// The matching tool_call is pruned from the preceding `Assistant`;
/// if that leaves `tool_calls` empty the assistant message goes too,
/// taking any narration with it. The plan's squash example deletes
/// intermediate "I'll try X" text — it's misleading once the actual
/// step that ran chose a different path.
///
/// `start` is the pre-turn message count — we never touch history
/// from earlier turns.
fn squash_denied_attempts(messages: &mut Vec<lutin_llm::Message>, start: usize) {
    use lutin_llm::Message;
    let mut i = messages.len();
    while i > start {
        i -= 1;
        let denied_call_id = match &messages[i] {
            Message::ToolResult(tr) if tr.is_error && review::is_review_denial(&tr.content) => {
                Some(tr.call_id.clone())
            }
            _ => None,
        };
        let Some(call_id) = denied_call_id else { continue };
        messages.remove(i);
        if i == start {
            continue;
        }
        let j = i - 1;
        let drop_assistant = match &mut messages[j] {
            Message::Assistant { tool_calls, .. } => {
                tool_calls.retain(|c| c.id != call_id);
                tool_calls.is_empty()
            }
            _ => false,
        };
        if drop_assistant {
            messages.remove(j);
            i = j;
        }
    }
}

fn sync_new_entries(agent_messages: &[lutin_llm::Message], entries: &mut Vec<Entry>) {
    for msg in &agent_messages[entries.len()..] {
        let tools = match msg {
            lutin_llm::Message::Assistant { tool_calls, .. } => {
                vec![ToolStats::default(); tool_calls.len()]
            }
            _ => Vec::new(),
        };
        entries.push(Entry {
            message: msg.clone(),
            metrics: MessageMetrics {
                timestamp: Some(now_rfc3339()),
                tools,
                ..Default::default()
            },
        });
    }
}

/// Run pre-turn compaction when the persona enables it. On a successful
/// compaction the agent's `messages` are spliced in place by
/// [`maybe_compact`]; we mirror the splice into `entries` (so metrics
/// stay aligned), append a snapshot of the dropped messages to a
/// per-session archive sidecar, persist the new transcript, and broadcast
/// `HistoryReplaced` + `MetricsReplaced` so the UI can rerender.
async fn run_compaction(
    ctx: &RunnerCtx,
    agent: &mut Agent,
    resolved: &ResolvedArgs,
    entries: &mut Vec<Entry>,
) {
    let Some(cfg) = CompactionConfig::from_persona(&resolved.persona) else {
        return;
    };
    let Some(provider_name) = resolved.persona.provider.as_deref() else {
        return;
    };
    let Some(provider_cfg) = resolved
        .settings
        .providers
        .iter()
        .find(|p| p.name == provider_name)
    else {
        warn!(provider = %provider_name, "compaction skipped: provider not configured");
        return;
    };
    let provider = match sdk_build_provider(provider_cfg) {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "compaction skipped: provider build failed");
            return;
        }
    };
    let Some(model) = resolved
        .model_override
        .clone()
        .or_else(|| resolved.persona.model.clone())
    else {
        warn!("compaction skipped: persona has no model");
        return;
    };
    let model_id = lutin_llm::ModelId::new(model);

    let outcome = match maybe_compact(agent, &*provider, &model_id, &cfg).await {
        Ok(Some(o)) => o,
        Ok(None) => return,
        Err(e) => {
            warn!(error = %e, "compaction failed; continuing with full transcript");
            return;
        }
    };

    apply_compaction_to_entries(entries, &outcome);
    if let Err(e) = append_compaction_archive(&ctx.state_dir, &outcome) {
        warn!(error = %e, "compaction archive write failed");
    }
    if let Err(e) = store::save(&ctx.state_dir, entries) {
        warn!(error = %e, "save transcript after compaction failed");
    }
    write_summary(ctx, entries);
    let _ = ctx
        .events
        .send(ChatEvent::HistoryReplaced(project_history(entries)));
    let _ = ctx
        .events
        .send(ChatEvent::MetricsReplaced(project_metrics(entries)));
    let _ = ctx.events.send(build_summary_updated(entries));
    info!(
        kept = outcome.kept,
        archived = outcome.archived_prefix.len(),
        "compaction applied"
    );
}

/// Mirror the agent-side splice into `entries` so metrics align. The
/// summary entry gets a fresh timestamp and otherwise-empty metrics.
fn apply_compaction_to_entries(entries: &mut Vec<Entry>, outcome: &CompactionOutcome) {
    let start = outcome.summarize_range_start;
    let end = start + outcome.archived_prefix.len();
    if end > entries.len() {
        warn!(
            entries_len = entries.len(),
            end, "compaction range exceeds entries — skipping mirror splice"
        );
        return;
    }
    let summary_entry = Entry {
        message: lutin_llm::Message::Summary { text: outcome.summary.clone() },
        metrics: MessageMetrics {
            timestamp: Some(now_rfc3339()),
            ..Default::default()
        },
    };
    entries.splice(start..end, std::iter::once(summary_entry));
}

/// Append one compaction event to `<state_dir>/compaction_archive.json`.
/// File is a JSON array; each element preserves the summary text and the
/// raw archived messages so users can audit what was dropped.
fn append_compaction_archive(
    state_dir: &std::path::Path,
    outcome: &CompactionOutcome,
) -> std::io::Result<()> {
    #[derive(Serialize, serde::Deserialize)]
    struct Archived {
        at: String,
        summary: String,
        messages: Vec<lutin_llm::Message>,
    }
    let path = state_dir.join("compaction_archive.json");
    let mut all: Vec<Archived> = match std::fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(e) => {
                // Don't silently overwrite an unreadable archive — that would
                // destroy the user's audit trail. Move it aside with a
                // timestamp so the next write starts fresh and the original
                // bytes stay recoverable on disk.
                let aside = state_dir.join(format!(
                    "compaction_archive.corrupt-{}.json",
                    now_rfc3339().replace(':', "-")
                ));
                warn!(error = %e, original = %path.display(), preserved_at = %aside.display(),
                      "compaction archive unreadable; rotated aside");
                std::fs::rename(&path, &aside)?;
                Vec::new()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    all.push(Archived {
        at: now_rfc3339(),
        summary: outcome.summary.clone(),
        messages: outcome.archived_prefix.clone(),
    });
    let body = serde_json::to_vec_pretty(&all)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = state_dir.join("compaction_archive.json.tmp");
    std::fs::write(&tmp, &body)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Attach turn stats to entries added during this turn. The last
/// `Message::Assistant` in `pre_turn_len..` gets the full set (TTFT,
/// duration, tokens); intermediates keep just their timestamp. Tool
/// stats are populated by walking the tracker's lifecycles and
/// resolving each `call_id` back to its slot in an assistant entry's
/// `tool_calls`.
fn finalize_turn_meta(entries: &mut Vec<Entry>, pre_turn_len: usize, tracker: &TurnTracker) {
    let now = Instant::now();
    let duration_ms = now.saturating_duration_since(tracker.started_at).as_millis() as u64;
    let ttft_ms = tracker
        .first_text_at
        .map(|t1| t1.saturating_duration_since(tracker.started_at).as_millis() as u64);
    let thinking_ttft_ms = tracker
        .first_thinking_at
        .map(|t1| t1.saturating_duration_since(tracker.started_at).as_millis() as u64);
    let (prompt_tokens, completion_tokens) = match &tracker.last_usage {
        Some(u) => (Some(u.prompt_tokens), Some(u.completion_tokens)),
        None => (None, None),
    };

    // Tool lifecycles: write each into the assistant entry that owns
    // the matching tool_call. Searching from `pre_turn_len` is
    // sufficient — calls only attach to assistants from this turn.
    for life in &tracker.tools {
        let dur = life
            .finished_at
            .map(|t| t.saturating_duration_since(life.started_at).as_millis() as u64);
        let stats = ToolStats {
            timestamp: Some(life.started_ts.clone()),
            duration_ms: dur,
        };
        if let Some((entry_idx, slot)) = locate_tool_slot(entries, &life.call_id, pre_turn_len) {
            if let Some(out) = entries[entry_idx].metrics.tools.get_mut(slot) {
                *out = stats;
            }
        }
    }

    // Last-assistant gets text/thinking stats.
    let last_assistant_idx = (pre_turn_len..entries.len())
        .rev()
        .find(|&i| matches!(entries[i].message, lutin_llm::Message::Assistant { .. }));
    let Some(idx) = last_assistant_idx else { return };
    let lutin_llm::Message::Assistant { text, thinking, .. } = &entries[idx].message else {
        return;
    };
    let has_text = !text.is_empty();
    let has_thinking = thinking.as_deref().is_some_and(|s| !s.is_empty());
    let metrics = &mut entries[idx].metrics;
    if has_text {
        metrics.text = Some(TextStats {
            ttft_ms,
            duration_ms: Some(duration_ms),
            prompt_tokens,
            completion_tokens,
        });
    }
    if has_thinking {
        metrics.thinking = Some(ThinkingStats {
            ttft_ms: thinking_ttft_ms,
            duration_ms: Some(duration_ms),
        });
    }
}

/// Find the assistant entry that owns a tool call with `call_id`,
/// returning its `(entry_index, slot_within_tool_calls)`. Searches
/// only from `start` so we don't pick up an old call with a recycled
/// id (rare but possible).
fn locate_tool_slot(entries: &[Entry], call_id: &str, start: usize) -> Option<(usize, usize)> {
    for (i, e) in entries.iter().enumerate().skip(start) {
        if let lutin_llm::Message::Assistant { tool_calls, .. } = &e.message
            && let Some(pos) = tool_calls.iter().position(|c| c.id.as_str() == call_id)
        {
            return Some((i, pos));
        }
    }
    None
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
    persona: Option<String>,
    model: Option<String>,
    total_prompt_tokens: Option<u64>,
    total_completion_tokens: Option<u64>,
    context_tokens: Option<u32>,
    message_count: Option<u32>,
}

const SUMMARY_TITLE_CHARS: usize = 80;
const SUMMARY_PREVIEW_CHARS: usize = 160;

/// Build + atomically write `<state_dir>/summary.json`. Called after
/// every turn so the dormant-session label tracks the latest state;
/// also called once at runner startup so the file exists before any
/// turns happen. Failures log a warning but never bubble — a missing
/// summary just means the chrome shows a generic fallback label, not
/// that the session is broken.
fn write_summary(ctx: &RunnerCtx, entries: &[Entry]) {
    let state_dir = ctx.state_dir.as_path();
    // Best-effort enrichment: persona name from session state, model
    // resolved through the persona TOML. Both are optional; failures
    // here just leave the fields blank in the summary file.
    let session_state = load_state(state_dir).ok();
    let persona_name = session_state.as_ref().and_then(|s| s.persona.clone());
    let model_override = session_state.as_ref().and_then(|s| s.model_override.clone());
    let resolved_model = model_override.or_else(|| {
        let name = persona_name.as_deref()?;
        Persona::load(&ctx.resolver, name).ok().and_then(|p| p.model)
    });
    let summary = build_summary(entries, persona_name, resolved_model);
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

fn build_summary(
    entries: &[Entry],
    persona: Option<String>,
    model: Option<String>,
) -> ChatSummary {
    let title = entries.iter().find_map(|e| match &e.message {
        lutin_llm::Message::User(text) if !text.trim().is_empty() => {
            Some(truncate_chars(text.trim(), SUMMARY_TITLE_CHARS))
        }
        _ => None,
    });
    let preview = entries.iter().rev().find_map(|e| match &e.message {
        lutin_llm::Message::Assistant { text, .. } if !text.trim().is_empty() => {
            Some(truncate_chars(text.trim(), SUMMARY_PREVIEW_CHARS))
        }
        _ => None,
    });
    let visible = entries
        .iter()
        .filter(|e| {
            matches!(
                &e.message,
                lutin_llm::Message::User(t) if !t.is_empty(),
            ) || matches!(
                &e.message,
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
    // Token totals: sum across every assistant entry's text stats. The
    // last assistant's prompt_tokens stands in for "current context
    // window fill" — it's the count the provider just charged for, so
    // it tracks the live transcript size after compaction too.
    let mut total_prompt: u64 = 0;
    let mut total_completion: u64 = 0;
    let mut last_prompt: Option<u32> = None;
    for e in entries {
        if let Some(t) = e.metrics.text {
            if let Some(p) = t.prompt_tokens {
                total_prompt = total_prompt.saturating_add(p as u64);
                last_prompt = Some(p);
            }
            if let Some(c) = t.completion_tokens {
                total_completion = total_completion.saturating_add(c as u64);
            }
        }
    }
    let total_prompt_tokens = (total_prompt > 0).then_some(total_prompt);
    let total_completion_tokens = (total_completion > 0).then_some(total_completion);

    ChatSummary {
        title,
        subtitle,
        last_activity: Some(chrono::Utc::now().to_rfc3339()),
        preview,
        persona,
        model,
        total_prompt_tokens,
        total_completion_tokens,
        context_tokens: last_prompt,
        message_count: Some(visible as u32),
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
fn project_history(entries: &[Entry]) -> Vec<HistoricalMessage> {
    project_messages(entries.iter().map(|e| &e.message))
}

/// Same projection rules as [`project_history`], but driven from raw
/// messages (not `Entry`s) so sub-agent transcripts — which never sit
/// in the engine's `Vec<Entry>` — can reuse the exact same widget
/// shape. Pairing of `tool_calls` to their `ToolResult` happens via a
/// linear scan keyed on `call_id`.
fn project_messages<'a>(
    messages: impl IntoIterator<Item = &'a lutin_llm::Message> + Clone,
) -> Vec<HistoricalMessage> {
    let mut results_by_id: Vec<(&str, &lutin_llm::ToolResultContent)> = Vec::new();
    for m in messages.clone() {
        if let lutin_llm::Message::ToolResult(tr) = m {
            results_by_id.push((tr.call_id.as_str(), tr));
        }
    }
    let mut out: Vec<HistoricalMessage> = Vec::new();
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
            lutin_llm::Message::Summary { text } => {
                out.push(HistoricalMessage::Summary { text: text.clone() });
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
) -> Option<FinishReason> {
    match ev {
        AgentEvent::AssistantText(s) => {
            if tracker.first_text_at.is_none() {
                tracker.first_text_at = Some(Instant::now());
            }
            let _ = events.send(ChatEvent::Delta(s));
            None
        }
        AgentEvent::AssistantReasoning(s) => {
            if tracker.first_thinking_at.is_none() {
                tracker.first_thinking_at = Some(Instant::now());
            }
            let _ = events.send(ChatEvent::Reasoning(s));
            None
        }
        AgentEvent::ToolCallStreaming { id, name } => {
            let _ = events.send(ChatEvent::ToolCallStreaming {
                id: id.as_str().to_string(),
                name: name.as_str().to_string(),
            });
            None
        }
        AgentEvent::ToolCallArgsDelta { id, args } => {
            let _ = events.send(ChatEvent::ToolCallArgsDelta {
                id: id.as_str().to_string(),
                args,
            });
            None
        }
        AgentEvent::ToolCallArgsParsed(call) => {
            tracker.tools.push(ToolLifecycle {
                call_id: call.id.as_str().to_string(),
                started_at: Instant::now(),
                started_ts: now_rfc3339(),
                finished_at: None,
            });
            // `Value` serialization is infallible — every variant has
            // a deterministic textual form. The TS decoder parses the
            // resulting JSON once at the wire boundary so downstream
            // sees a parsed value, not a string.
            let arguments_json = serde_json::to_string(&call.arguments)
                .expect("serializing serde_json::Value is infallible");
            let _ = events.send(ChatEvent::ToolCallArgsParsed {
                id: call.id.as_str().to_string(),
                name: call.name.as_str().to_string(),
                arguments_json,
            });
            None
        }
        AgentEvent::ToolCallCompleted { call, outcome } => {
            if let Some(life) = tracker
                .tools
                .iter_mut()
                .rev()
                .find(|l| l.call_id == call.id.as_str())
            {
                life.finished_at = Some(Instant::now());
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
            tracker.intra_turn_prompt = tracker
                .intra_turn_prompt
                .saturating_add(u.prompt_tokens as u64);
            tracker.intra_turn_completion = tracker
                .intra_turn_completion
                .saturating_add(u.completion_tokens as u64);
            let _ = events.send(ChatEvent::SummaryUpdated {
                context_tokens: Some(u.prompt_tokens),
                total_prompt_tokens: tracker
                    .total_prompt_pre_turn
                    .saturating_add(tracker.intra_turn_prompt),
                total_completion_tokens: tracker
                    .total_completion_pre_turn
                    .saturating_add(tracker.intra_turn_completion),
            });
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

/// Project entries to wire `MessageMeta` aligned 1:1 with `project_history`.
fn project_metrics(entries: &[Entry]) -> Vec<MessageMeta> {
    let mut out: Vec<MessageMeta> = Vec::with_capacity(entries.len());
    for entry in entries {
        let ts = entry.metrics.timestamp.clone();
        match &entry.message {
            lutin_llm::Message::User(text) if !text.is_empty() => {
                out.push(MessageMeta::User { timestamp: ts });
            }
            lutin_llm::Message::SubAgentReply { .. } => {
                out.push(MessageMeta::SubAgentReply { timestamp: ts });
            }
            lutin_llm::Message::SubAgentFailure { .. } => {
                out.push(MessageMeta::SubAgentFailure { timestamp: ts });
            }
            lutin_llm::Message::Summary { .. } => {
                out.push(MessageMeta::Summary { timestamp: ts });
            }
            lutin_llm::Message::Assistant { text, thinking, tool_calls } => {
                if thinking.as_deref().is_some_and(|s| !s.is_empty()) {
                    let s = entry.metrics.thinking.unwrap_or_default();
                    out.push(MessageMeta::Thinking {
                        timestamp: ts.clone(),
                        ttft_ms: s.ttft_ms,
                        duration_ms: s.duration_ms,
                    });
                }
                if !text.is_empty() {
                    let s = entry.metrics.text.unwrap_or_default();
                    out.push(MessageMeta::Assistant {
                        timestamp: ts.clone(),
                        ttft_ms: s.ttft_ms,
                        duration_ms: s.duration_ms,
                        prompt_tokens: s.prompt_tokens,
                        completion_tokens: s.completion_tokens,
                    });
                }
                for (i, _call) in tool_calls.iter().enumerate() {
                    let stats = entry.metrics.tools.get(i).cloned().unwrap_or_default();
                    out.push(MessageMeta::Tool {
                        timestamp: stats.timestamp,
                        duration_ms: stats.duration_ms,
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
        AgentFinishReason::Stopped => FinishReason::Completed,
        AgentFinishReason::MaxRounds => FinishReason::MaxRounds,
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
    /// Resolved per-session reviewer fan-out concurrency. Already
    /// defaulted + clamped via `SessionState::review_concurrency`.
    review_concurrency: usize,
}

impl ResolvedArgs {
    /// Bind the resolved inputs to the SDK's build interface. Sub-agent
    /// tools are constructed fresh per call (each one closes over a
    /// clone of the registry sender from `ctx`); the persona's filter
    /// then drops them for non-orchestrator personas — see
    /// `tools::agent` for the gating story.
    fn as_build_args(&self, ctx: &RunnerCtx) -> BuildArgs<'_> {
        self.as_build_args_with(ctx, PromptExtras::default(), None)
    }

    fn as_build_args_with(
        &self,
        ctx: &RunnerCtx,
        prompt_extras: PromptExtras,
        owner_id: Option<agents::AgentId>,
    ) -> BuildArgs<'_> {
        BuildArgs {
            persona: &self.persona,
            settings: &self.settings,
            sandbox_root: self.sandbox_root.clone(),
            model_override: self.model_override.clone(),
            extra_tools: tools::make_subagent_tools(
                ctx.agent_registry.clone(),
                ctx.resolver.clone(),
                owner_id,
            ),
            prompt_extras,
        }
    }
}

/// Resolve the chat-specific inputs the SDK needs from on-disk state.
/// Translates SDK-agnostic errors (file IO, persona-not-found) back to
/// the chat protocol's typed variants.
///
/// `persona_override` lets a caller skip the disk read by handing in
/// an already-loaded `Persona` (sub-agent spawn path — the tool
/// boundary validated and loaded it once). `None` reads the session's
/// configured persona from disk; this is the main-session path that
/// honours out-of-band edits.
fn resolve_args(
    ctx: &RunnerCtx,
    persona_override: Option<Persona>,
) -> Result<ResolvedArgs, ChatError> {
    let session_state = load_state(&ctx.state_dir)
        .map_err(|e| ChatError::Internal(format!("load state: {e}")))?;

    let persona = match persona_override {
        Some(p) => p,
        None => {
            let name = session_state.persona.as_deref().unwrap_or(DEFAULT_PERSONA);
            Persona::load(&ctx.resolver, name).map_err(|e| match e {
                lutin_entities::EntityError::NotFound { name, .. } => {
                    ChatError::PersonaNotFound(name)
                }
                other => ChatError::Internal(format!("load persona: {other}")),
            })?
        }
    };
    let settings = Settings::load(&ctx.resolver)
        .map_err(|e| ChatError::Internal(format!("load settings: {e}")))?;

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

    let review_concurrency = session_state.review_concurrency();
    Ok(ResolvedArgs {
        persona,
        settings,
        sandbox_root,
        model_override: session_state.model_override,
        review_concurrency,
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


#[cfg(test)]
mod tests {
    //! Behavior tests for the metrics/transcript machinery. These
    //! exercise `finalize_turn_meta`, `mutate_entries`, and the two
    //! projection functions directly — wire-byte goldens live in
    //! `lib.rs::tests::golden_postcard_bytes` and cover the encode
    //! shape, not the semantic invariants checked here.
    use super::*;
    use lutin_llm::{Message, ToolCall};

    fn assistant(text: &str) -> Message {
        Message::Assistant {
            text: text.into(),
            thinking: None,
            tool_calls: Vec::new(),
        }
    }

    fn assistant_with_calls(text: &str, calls: Vec<ToolCall>) -> Message {
        Message::Assistant {
            text: text.into(),
            thinking: None,
            tool_calls: calls,
        }
    }

    fn entry(message: Message) -> Entry {
        Entry { message, metrics: MessageMetrics::default() }
    }

    #[test]
    fn finalize_turn_meta_attributes_to_last_assistant() {
        // Two assistants in a turn (a tool round followed by a final
        // text round). Token totals + duration land on the LAST
        // assistant; the intermediate keeps just its timestamp.
        let mut entries = vec![
            entry(Message::User("hi".into())),
            entry(assistant("first")),
            entry(assistant("second")),
        ];
        let mut tracker = TurnTracker::new(sdk_summary::SummaryTotals::default());
        // Manually rewind started_at so duration reads as something
        // > 0 even on fast machines.
        tracker.started_at = std::time::Instant::now() - std::time::Duration::from_millis(123);
        tracker.first_text_at = Some(std::time::Instant::now() - std::time::Duration::from_millis(80));
        tracker.last_usage = Some(lutin_llm::Usage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
        });
        finalize_turn_meta(&mut entries, 1, &tracker);
        assert!(entries[1].metrics.text.is_none(), "intermediate has no stats");
        let last = entries[2].metrics.text.expect("last assistant has stats");
        assert_eq!(last.prompt_tokens, Some(100));
        assert_eq!(last.completion_tokens, Some(50));
        assert!(last.duration_ms.unwrap() >= 100);
        // first_text_at − started_at = 123 − 80 = 43 ms; allow slop.
        assert!(last.ttft_ms.unwrap() >= 30);
        assert!(last.ttft_ms.unwrap() <= 80);
    }

    #[test]
    fn finalize_turn_meta_records_tool_durations_by_call_id() {
        let call = ToolCall {
            id: lutin_llm::CallId::new("c1"),
            name: lutin_llm::ToolName::new("read"),
            arguments: serde_json::json!({}),
        };
        let mut entries = vec![
            entry(Message::User("hi".into())),
            entry(assistant_with_calls("doing", vec![call])),
        ];
        // Pre-seed the assistant's `tools` slot so `finalize_turn_meta`
        // has a place to write into — the runtime path does this in
        // `sync_new_entries`. Length must match `tool_calls`.
        entries[1].metrics.tools = vec![ToolStats::default()];
        let mut tracker = TurnTracker::new(sdk_summary::SummaryTotals::default());
        let now = std::time::Instant::now();
        tracker.tools.push(ToolLifecycle {
            call_id: "c1".into(),
            started_at: now - std::time::Duration::from_millis(200),
            started_ts: "2026-05-08T12:00:00Z".into(),
            finished_at: Some(now - std::time::Duration::from_millis(50)),
        });
        finalize_turn_meta(&mut entries, 1, &tracker);
        let stats = &entries[1].metrics.tools[0];
        assert_eq!(stats.timestamp.as_deref(), Some("2026-05-08T12:00:00Z"));
        assert!(stats.duration_ms.unwrap() >= 100);
    }

    #[test]
    fn mutate_entries_delete_user_drops_entry_and_metrics_together() {
        let mut entries = vec![
            entry(Message::User("first".into())),
            entry(Message::User("second".into())),
        ];
        entries[0].metrics.timestamp = Some("t0".into());
        entries[1].metrics.timestamp = Some("t1".into());
        mutate_entries(&mut entries, &MutateOp::Delete { index: 0 }).unwrap();
        assert_eq!(entries.len(), 1);
        // The remaining entry is the originally-second user; its
        // metrics moved with it.
        assert_eq!(entries[0].metrics.timestamp.as_deref(), Some("t1"));
    }

    #[test]
    fn mutate_entries_delete_from_truncates_both_halves() {
        let mut entries = vec![
            entry(Message::User("a".into())),
            entry(Message::User("b".into())),
            entry(Message::User("c".into())),
        ];
        entries[2].metrics.timestamp = Some("t2".into());
        mutate_entries(&mut entries, &MutateOp::DeleteFrom { index: 1 }).unwrap();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn mutate_entries_edit_user_text() {
        let mut entries = vec![entry(Message::User("old".into()))];
        mutate_entries(
            &mut entries,
            &MutateOp::Edit { index: 0, text: "new".into() },
        )
        .unwrap();
        match &entries[0].message {
            Message::User(t) => assert_eq!(t, "new"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn mutate_entries_rejects_tool_edit() {
        let call = ToolCall {
            id: lutin_llm::CallId::new("c1"),
            name: lutin_llm::ToolName::new("read"),
            arguments: serde_json::json!({}),
        };
        let mut entries = vec![entry(assistant_with_calls("calling", vec![call]))];
        // Projected slots: AssistantText (idx 0) + Tool (idx 1).
        let result = mutate_entries(
            &mut entries,
            &MutateOp::Edit { index: 1, text: "no".into() },
        );
        assert!(matches!(result, Err(ChatError::HistoryIndexOutOfRange(1))));
    }

    #[test]
    fn project_metrics_aligns_with_project_history() {
        let call = ToolCall {
            id: lutin_llm::CallId::new("c1"),
            name: lutin_llm::ToolName::new("read"),
            arguments: serde_json::json!({}),
        };
        let entries = vec![
            Entry {
                message: Message::User("hi".into()),
                metrics: MessageMetrics {
                    timestamp: Some("t0".into()),
                    ..Default::default()
                },
            },
            Entry {
                message: assistant_with_calls("hello", vec![call]),
                metrics: MessageMetrics {
                    timestamp: Some("t1".into()),
                    text: Some(TextStats {
                        ttft_ms: Some(10),
                        duration_ms: Some(50),
                        prompt_tokens: Some(5),
                        completion_tokens: Some(3),
                    }),
                    tools: vec![ToolStats {
                        timestamp: Some("t1.5".into()),
                        duration_ms: Some(30),
                    }],
                    ..Default::default()
                },
            },
        ];
        let history = project_history(&entries);
        let metrics = project_metrics(&entries);
        assert_eq!(history.len(), metrics.len(), "1:1 alignment");
        assert!(matches!(metrics[0], MessageMeta::User { .. }));
        assert!(matches!(metrics[1], MessageMeta::Assistant { .. }));
        assert!(matches!(metrics[2], MessageMeta::Tool { .. }));
    }

    fn tool_call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: lutin_llm::CallId::new(id),
            name: lutin_llm::ToolName::new(name),
            arguments: serde_json::json!({}),
        }
    }

    fn tool_result(call_id: &str, content: &str, is_error: bool) -> Message {
        Message::ToolResult(lutin_llm::ToolResultContent {
            call_id: lutin_llm::CallId::new(call_id),
            content: content.into(),
            is_error,
        })
    }

    #[test]
    fn squash_drops_denied_pair_and_keeps_accepted() {
        let mut msgs = vec![
            Message::User("hi".into()),
            assistant_with_calls("v1", vec![tool_call("c1", "edit")]),
            tool_result("c1", "denied: <review-deny> rejected by 'X': bad", true),
            assistant_with_calls("v2", vec![tool_call("c2", "edit")]),
            tool_result("c2", "applied", false),
        ];
        squash_denied_attempts(&mut msgs, 1);
        assert_eq!(msgs.len(), 3);
        match &msgs[1] {
            Message::Assistant { tool_calls, text, .. } => {
                assert_eq!(text, "v2");
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].id.as_str(), "c2");
            }
            _ => panic!("expected assistant"),
        }
        match &msgs[2] {
            Message::ToolResult(tr) => assert_eq!(tr.call_id.as_str(), "c2"),
            _ => panic!("expected tool result"),
        }
    }

    #[test]
    fn squash_preserves_assistant_when_other_tool_calls_remain() {
        let mut msgs = vec![
            assistant_with_calls(
                "multi",
                vec![tool_call("c1", "edit"), tool_call("c2", "read")],
            ),
            tool_result("c1", "denied: <review-deny> rejected by 'X': bad", true),
            tool_result("c2", "ok", false),
        ];
        squash_denied_attempts(&mut msgs, 0);
        assert_eq!(msgs.len(), 2);
        match &msgs[0] {
            Message::Assistant { tool_calls, .. } => {
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].id.as_str(), "c2");
            }
            _ => panic!("expected assistant"),
        }
    }

    #[test]
    fn squash_ignores_real_tool_errors() {
        // A non-denied error is a normal tool failure — keep it. Only
        // the `denied:` prefix is the review-loop signal.
        let mut msgs = vec![
            assistant_with_calls("v1", vec![tool_call("c1", "edit")]),
            tool_result("c1", "file not found", true),
        ];
        let before_len = msgs.len();
        squash_denied_attempts(&mut msgs, 0);
        assert_eq!(msgs.len(), before_len);
        match &msgs[1] {
            Message::ToolResult(tr) => {
                assert!(tr.is_error);
                assert_eq!(tr.content, "file not found");
            }
            _ => panic!("expected tool result"),
        }
    }

    #[test]
    fn squash_does_not_touch_pre_turn_history() {
        // A `denied:` message older than `start` stays — squash only
        // operates on this turn's window.
        let mut msgs = vec![
            assistant_with_calls("old", vec![tool_call("c0", "edit")]),
            tool_result("c0", "denied: rejected by 'old': bad", true),
            Message::User("new turn".into()),
            assistant_with_calls("v1", vec![tool_call("c1", "edit")]),
            tool_result("c1", "applied", false),
        ];
        squash_denied_attempts(&mut msgs, 2);
        assert_eq!(msgs.len(), 5, "pre-turn portion untouched");
    }
}
