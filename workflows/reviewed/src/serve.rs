//! WebSocket connection handler. hello → auth → dispatch loop.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use futures_util::{SinkExt, StreamExt, stream::SplitSink};
use lutin_auth::{Scope, SessionId, Slug, VerifyingKey, WorkflowId, verify};
use lutin_entities::Persona;
use lutin_protocol::{Frame, HandshakeResult, PROTOCOL_VERSION, decode, encode};
use lutin_storage::Resolver;
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tracing::warn;

use crate::runner::AgentCmd;
use crate::store;
use crate::wire::{
    ChatError, ChatEvent, ChatOk, ChatRequest, ChatResponse, PersonaInfo, SessionState, Turn,
    TurnId, decode as wire_decode, encode as wire_encode,
};

const SESSION_FILE: &str = "session.toml";

#[derive(Clone)]
pub struct AppState {
    pub project: Slug,
    pub workflow: WorkflowId,
    pub session: SessionId,
    pub issuer: VerifyingKey,
    pub state_dir: PathBuf,
    pub resolver: Arc<Resolver>,
    pub events: broadcast::Sender<ChatEvent>,
    pub next_turn: Arc<AtomicU64>,
    pub agent_cmds: mpsc::UnboundedSender<AgentCmd>,
}

impl AppState {
    fn next_turn(&self) -> TurnId {
        TurnId(self.next_turn.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SessionFile {
    persona: Option<String>,
}

fn session_path(state_dir: &Path) -> PathBuf {
    state_dir.join(SESSION_FILE)
}

pub(crate) fn load_session(state_dir: &Path) -> Result<SessionState, ChatError> {
    let path = session_path(state_dir);
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(SessionState::default()),
        Err(e) => return Err(ChatError::Internal { message: format!("read {}: {e}", path.display()) }),
    };
    let parsed: SessionFile = toml::from_str(&raw)
        .map_err(|e| ChatError::Internal { message: format!("parse {}: {e}", path.display()) })?;
    Ok(SessionState { persona: parsed.persona })
}

fn save_session(state_dir: &Path, s: &SessionState) -> Result<(), ChatError> {
    let path = session_path(state_dir);
    let file = SessionFile { persona: s.persona.clone() };
    let body = toml::to_string_pretty(&file)
        .map_err(|e| ChatError::Internal { message: format!("serialise session: {e}") })?;
    std::fs::create_dir_all(state_dir).map_err(|e| ChatError::Internal {
        message: format!("create {}: {e}", state_dir.display()),
    })?;
    let tmp = state_dir.join(format!("{SESSION_FILE}.tmp"));
    std::fs::write(&tmp, body).map_err(|e| ChatError::Internal {
        message: format!("write {}: {e}", tmp.display()),
    })?;
    std::fs::rename(&tmp, &path).map_err(|e| ChatError::Internal {
        message: format!("rename {}: {e}", path.display()),
    })?;
    Ok(())
}

/// Replay the final, post-rewind message history into UI turns.
/// Failed draft attempts have already been stripped from `messages`
/// by the runtime, so this is just a straight 1:1 projection.
fn project_turns(messages: &[lutin_llm::Message]) -> Vec<Turn> {
    let mut turns = Vec::new();
    let mut user_idx = 0u64;
    let mut assistant_idx = 0u64;
    let mut tool_idx = 0u64;

    // Pair Assistant(tool_call) with the following ToolResult by call_id.
    let mut pending: Option<(String, String, String)> = None; // (call_id, tool, args)
    for msg in messages {
        match msg {
            lutin_llm::Message::User(text) => {
                turns.push(Turn::User {
                    id: format!("u-{user_idx}"),
                    text: text.clone(),
                });
                user_idx += 1;
            }
            lutin_llm::Message::Assistant { text, tool_calls, .. } => {
                if let Some(tc) = tool_calls.first() {
                    let args =
                        serde_json::to_string(&tc.arguments).unwrap_or_else(|_| "{}".into());
                    pending = Some((tc.id.as_str().to_string(), tc.name.as_str().to_string(), args));
                } else if !text.is_empty() {
                    turns.push(Turn::Assistant {
                        id: format!("a-{assistant_idx}"),
                        text: text.clone(),
                    });
                    assistant_idx += 1;
                }
            }
            lutin_llm::Message::ToolResult(rc) => {
                if let Some((call_id, tool, args)) = pending.take()
                    && call_id == rc.call_id.as_str()
                {
                    turns.push(Turn::ToolCall {
                        id: format!("t-{tool_idx}"),
                        tool,
                        args,
                        output: rc.content.clone(),
                    });
                    tool_idx += 1;
                }
            }
            _ => {}
        }
    }
    turns
}

type WsSink = SplitSink<WebSocketStream<TcpStream>, Message>;

async fn send_nack(tx: &mut WsSink, reason: &str) -> Result<()> {
    let nack = encode(&Frame::HelloAck(HandshakeResult::Rejected {
        reason: reason.to_string(),
    }))?;
    tx.send(Message::Binary(nack.into())).await?;
    Ok(())
}

pub async fn serve_conn(sock: TcpStream, state: AppState) -> Result<()> {
    let ws = tokio_tungstenite::accept_async(sock).await?;
    let (mut tx, mut rx) = ws.split();

    let Some(msg) = rx.next().await else {
        return Ok(());
    };
    let bytes = match msg? {
        Message::Binary(b) => b,
        _ => anyhow::bail!("expected binary hello"),
    };
    let Frame::Hello { protocol_version, token } = decode(&bytes)? else {
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
            Scope::WorkflowSession { project, workflow, session }
                if project == &state.project
                    && workflow == &state.workflow
                    && session == &state.session => {}
            _ => return send_nack(&mut tx, "scope mismatch for this workflow session").await,
        },
        Err(e) => return send_nack(&mut tx, &format!("auth: {e}")).await,
    }
    let ack = encode(&Frame::HelloAck(HandshakeResult::Accepted))?;
    tx.send(Message::Binary(ack.into())).await?;

    let mut events = state.events.subscribe();

    loop {
        tokio::select! {
            biased;
            ev = events.recv() => match ev {
                Ok(e) => {
                    let body = wire_encode(&e)?;
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
                        let req = wire_decode::<ChatRequest>(&body)?;
                        let resp = handle_request(&state, req).await;
                        let body = wire_encode(&resp)?;
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
            let s = load_session(&state.state_dir)?;
            let turns = match store::load(&state.state_dir) {
                Ok(Some(saved)) => project_turns(&saved.messages),
                Ok(None) => Vec::new(),
                Err(e) => {
                    return Err(ChatError::Internal {
                        message: format!("load state: {e}"),
                    });
                }
            };
            Ok(ChatOk::Subscribed { state: s, turns })
        }
        ChatRequest::GetState => Ok(ChatOk::State { state: load_session(&state.state_dir)? }),
        ChatRequest::SetPersona { name } => {
            if let Some(n) = &name
                && Persona::load(&state.resolver, n).is_err()
            {
                return Err(ChatError::PersonaNotFound { name: n.clone() });
            }
            let s = SessionState { persona: name };
            save_session(&state.state_dir, &s)?;
            let _ = state.events.send(ChatEvent::StateChanged { state: s.clone() });
            Ok(ChatOk::StateUpdated { state: s })
        }
        ChatRequest::ListPersonas => {
            let personas = Persona::list(&state.resolver)
                .map_err(|e| ChatError::Internal { message: format!("list personas: {e}") })?
                .into_iter()
                .map(|p| PersonaInfo {
                    name: p.name,
                    display_name: p.display_name,
                    model: p.model.unwrap_or_default(),
                })
                .collect();
            Ok(ChatOk::Personas { personas })
        }
        ChatRequest::SendMessage { text } => {
            let turn = state.next_turn();
            if state.agent_cmds.send(AgentCmd::Send { text, turn }).is_err() {
                return Err(ChatError::Internal {
                    message: "agent runner unavailable".into(),
                });
            }
            Ok(ChatOk::MessageQueued { turn_id: turn })
        }
        ChatRequest::Cancel => {
            let _ = state.agent_cmds.send(AgentCmd::Cancel);
            Ok(ChatOk::Cancelled)
        }
    }
}
