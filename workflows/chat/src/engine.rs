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

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, anyhow};
use chat::{
    ChatError, ChatEvent, ChatOk, ChatRequest, ChatResponse, FinishReason, HistoricalMessage,
    HistoricalRole, SessionState, TurnId, decode as chat_decode, encode as chat_encode, load_state,
    save_state,
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
use tokio::sync::{broadcast, mpsc};
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

    let runner_ctx = RunnerCtx {
        state_dir: env.state_dir.clone(),
        global_config_dir: env.global_config_dir.clone(),
        project_config_dir: env.project_config_dir.clone(),
        events: events.clone(),
        failure: runner_failure.clone(),
    };
    tokio::spawn(run_agent_loop(runner_ctx, agent_rx));

    let state = AppState {
        project: env.project,
        workflow: env.workflow,
        session: env.session,
        issuer: env.project_pubkey,
        state_dir: env.state_dir,
        global_config_dir: env.global_config_dir,
        project_config_dir: env.project_config_dir,
        events,
        next_turn: Arc::new(AtomicU64::new(1)),
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

struct RunnerCtx {
    state_dir: PathBuf,
    global_config_dir: PathBuf,
    project_config_dir: PathBuf,
    events: broadcast::Sender<ChatEvent>,
    /// Shared with `AppState`. Runner writes the failure reason here
    /// before exiting so subsequent `Send` requests can return it
    /// instead of the placeholder "agent runner is gone".
    failure: Arc<Mutex<Option<String>>>,
}

impl RunnerCtx {
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

async fn run_agent_loop(ctx: RunnerCtx, mut rx: mpsc::UnboundedReceiver<AgentCmd>) {
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

    let mut agent: Option<Agent> = None;
    while let Some(cmd) = rx.recv().await {
        match cmd {
            AgentCmd::Send { text, turn } => {
                if let Some(a) = ensure_agent(&mut agent, &ctx, &history, turn) {
                    run_turn(&ctx, &mut rx, a, Some(text), turn).await;
                }
            }
            AgentCmd::Rerun { turn } => {
                if let Some(a) = ensure_agent(&mut agent, &ctx, &history, turn) {
                    run_turn(&ctx, &mut rx, a, None, turn).await;
                }
            }
            AgentCmd::Cancel => {} // idle — nothing to cancel
        }
    }
}

/// Lazy-build the agent on first use; surface init failures as a
/// turn-level error so the runner stays alive. Returns `None` when
/// the build failed (and the caller should skip the turn).
fn ensure_agent<'a>(
    slot: &'a mut Option<Agent>,
    ctx: &RunnerCtx,
    history: &[lutin_llm::Message],
    turn: TurnId,
) -> Option<&'a mut Agent> {
    if slot.is_none() {
        match build_initial_agent(ctx, history) {
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

/// Build the agent on first use and seed its message vec with the
/// on-disk transcript. Returns a human-readable reason on failure so
/// the caller can surface it as a turn-level error and stay alive.
fn build_initial_agent(ctx: &RunnerCtx, history: &[lutin_llm::Message]) -> Result<Agent, String> {
    let resolved = resolve_args(ctx).map_err(|e| format!("resolve args: {e}"))?;
    let mut agent = sdk_build_agent(resolved.as_build_args())
        .map_err(|e| format!("build agent: {}", map_build_error(e)))?;
    agent
        .edit_messages(|m| *m = history.to_vec())
        .map_err(|e| format!("seed agent messages: {e}"))?;
    Ok(agent)
}

/// `text` is `Some(_)` for a new user message and `None` for a Rerun,
/// which kicks the agent loop against the existing transcript.
async fn run_turn(
    ctx: &RunnerCtx,
    rx: &mut mpsc::UnboundedReceiver<AgentCmd>,
    agent: &mut Agent,
    text: Option<String>,
    turn: TurnId,
) {
    // Refresh provider/model/sampling/system/tools from disk so
    // out-of-band edits to persona or settings take effect on this
    // turn. The agent's `messages` survive the swap.
    let resolved = match resolve_args(ctx) {
        Ok(r) => r,
        Err(e) => {
            let _ = ctx.events.send(ChatEvent::MessageFinished {
                turn_id: turn,
                reason: FinishReason::Failed(format!("{e}")),
            });
            return;
        }
    };
    if let Err(e) = sdk_refresh_agent(agent, resolved.as_build_args()) {
        let _ = ctx.events.send(ChatEvent::MessageFinished {
            turn_id: turn,
            reason: FinishReason::Failed(format!("{}", map_build_error(e))),
        });
        return;
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
    }
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
                    if let Some(reason) = handle_agent_event(ev, &ctx.events) {
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
                None => {
                    agent.cancel();
                }
            }
        }
    }

    let outcome = agent.join().await;
    // Single write per turn from the agent's own message vec — the
    // single source of truth. Even on Cancel/Failed, partials are
    // preserved so the user can see where it stopped.
    if let Err(e) = transcript::save(&ctx.state_dir, agent.messages()) {
        warn!(error = %e, "save transcript failed");
    }
    write_summary(&ctx.state_dir, agent.messages());
    let reason = finish.unwrap_or_else(|| map_finish_reason(outcome.finish_reason));
    let _ = ctx.events.send(ChatEvent::MessageFinished {
        turn_id: turn,
        reason,
    });
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

/// Project the engine-side `Vec<Message>` to the chat protocol's
/// UI-friendly shape. Drops `System` (already in persona prompt),
/// `ToolResult` (the chat UI doesn't render tool exchanges yet),
/// `Image` (not surfaced as a separate message in the UI), and any
/// `Assistant` whose text is empty (pure tool-call rounds). Order is
/// preserved.
fn project_history(messages: &[lutin_llm::Message]) -> Vec<HistoricalMessage> {
    let mut out = Vec::with_capacity(messages.len());
    for m in messages {
        match m {
            lutin_llm::Message::User(text) if !text.is_empty() => out.push(HistoricalMessage {
                role: HistoricalRole::User,
                text: text.clone(),
            }),
            lutin_llm::Message::Assistant { text, thinking, .. } => {
                if let Some(t) = thinking
                    && !t.is_empty()
                {
                    out.push(HistoricalMessage {
                        role: HistoricalRole::Thinking,
                        text: t.clone(),
                    });
                }
                if !text.is_empty() {
                    out.push(HistoricalMessage {
                        role: HistoricalRole::Assistant,
                        text: text.clone(),
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
) -> Option<FinishReason> {
    match ev {
        AgentEvent::AssistantText(s) => {
            let _ = events.send(ChatEvent::Delta(s));
            None
        }
        AgentEvent::AssistantReasoning(s) => {
            let _ = events.send(ChatEvent::Reasoning(s));
            None
        }
        AgentEvent::ToolCallStarted(call) => {
            let _ = events.send(ChatEvent::ToolCallStarted {
                id: call.id.as_str().to_string(),
                name: call.name.as_str().to_string(),
            });
            None
        }
        AgentEvent::ToolCallCompleted { call, outcome } => {
            let (ok, summary) = match outcome {
                ToolResult::Ok(c) => (!c.is_error, c.content),
                ToolResult::Err(e) => (false, format!("{e}")),
                other => {
                    warn!(?other, "unrecognized ToolResult variant");
                    (false, "unrecognized ToolResult variant".to_string())
                }
            };
            let _ = events.send(ChatEvent::ToolCallCompleted {
                id: call.id.as_str().to_string(),
                ok,
                summary,
            });
            None
        }
        AgentEvent::Finished(reason) => Some(map_finish_reason(reason)),
        AgentEvent::Error(e) => Some(FinishReason::Failed(format!("{e}"))),
        // Round/usage/full-message events aren't in the chat protocol yet.
        AgentEvent::RoundStarted { .. }
        | AgentEvent::RoundEnded { .. }
        | AgentEvent::AssistantMessage(_)
        | AgentEvent::Usage(_) => None,
        other => {
            warn!(?other, "unrecognized AgentEvent variant");
            None
        }
    }
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
    fn as_build_args(&self) -> BuildArgs<'_> {
        BuildArgs {
            persona: &self.persona,
            settings: &self.settings,
            sandbox_root: self.sandbox_root.clone(),
            model_override: self.model_override.clone(),
            extra_tools: Vec::new(),
        }
    }
}

/// Resolve the chat-specific inputs the SDK needs from on-disk state.
/// Translates SDK-agnostic errors (file IO, persona-not-found) back to
/// the chat protocol's typed variants.
fn resolve_args(ctx: &RunnerCtx) -> Result<ResolvedArgs, ChatError> {
    let session_state = load_state(&ctx.state_dir)
        .map_err(|e| ChatError::Internal(format!("load state: {e}")))?;

    let resolver = Resolver::new(
        ctx.global_config_dir.clone(),
        Some(ctx.project_config_dir.clone()),
    );

    let persona_name = session_state
        .persona
        .as_deref()
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
    // the trailing component to give `<root>/<slug>/`.
    let sandbox_root = ctx
        .project_config_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| ctx.project_config_dir.clone());

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

