//! WebSocket connection handler + request dispatch.
//!
//! Authenticates each connection with a `WorkflowSession`-scoped token
//! issued by the project tier, then bridges the bidirectional
//! event/request streams to/from the runner task.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use anyhow::Result;
use futures_util::{SinkExt, StreamExt, stream::SplitSink};
use lutin_auth::{Scope, SessionId, Slug, VerifyingKey, WorkflowId, verify};
use lutin_entities::Persona;
use lutin_protocol::{Frame, HandshakeResult, PROTOCOL_VERSION, decode, encode};
use lutin_storage::Resolver;
use principled::{
    ChatError, ChatEvent, ChatOk, ChatRequest, ChatResponse, SessionState, TurnId,
    decode as chat_decode, encode as chat_encode, load_state, save_state,
};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tracing::warn;

use crate::agents;
use crate::mutation::MutateOp;
use crate::projection::{project_history, project_messages, project_metrics};
use crate::review;
use crate::runner::AgentCmd;
use crate::store;
use crate::subagents_glue::project_summary;

#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) project: Slug,
    pub(crate) workflow: WorkflowId,
    pub(crate) session: SessionId,
    pub(crate) issuer: VerifyingKey,
    pub(crate) state_dir: PathBuf,
    pub(crate) resolver: Arc<Resolver>,
    pub(crate) events: broadcast::Sender<ChatEvent>,
    pub(crate) next_turn: Arc<AtomicU64>,
    pub(crate) agent_cmds: mpsc::UnboundedSender<AgentCmd>,
    pub(crate) runner_failure: watch::Receiver<Option<String>>,
    pub(crate) agent_registry: mpsc::UnboundedSender<agents::AgentRegistryCmd>,
}

impl AppState {
    pub(crate) fn next_turn(&self) -> TurnId {
        use std::sync::atomic::Ordering;
        TurnId(self.next_turn.fetch_add(1, Ordering::Relaxed))
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

pub(crate) async fn serve_conn(sock: TcpStream, state: AppState) -> Result<()> {
    let ws = tokio_tungstenite::accept_async(sock).await?;
    let (mut tx, mut rx) = ws.split();

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
