//! Project-tier server. WS endpoint, supervisor-owned session list,
//! request dispatch, broadcast fan-out. Mirrors control-panel shape.
//!
//! ## Lifetime contract
//!
//! The supervisor's session list is in-process state. A supervisor
//! restart (process crash, container restart) drops every session;
//! workflow runtimes live in this same process, so they die with it.
//! Clients learn via reconnect → `ListSessions`. Persistent state is
//! limited to the project's signing keypair, written to disk at
//! startup so token chains survive restarts.

pub mod build;
pub mod session;
pub mod workflows;

use std::path::PathBuf;

use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use lutin_auth::{Scope, SigningKey, Slug, VerifyingKey, pubkey_to_string, verify};
use lutin_project_protocol::{
    self as pp, ApiError, BuildOutcome, Event, Request, Response, ResponseOk, SessionEndpoint,
    SessionId, SessionInfo, WorkflowId,
};
use lutin_protocol::{Frame, HandshakeResult, PROTOCOL_VERSION, decode, encode};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tracing::warn;

use crate::session::{SessionHandle, default_session_dir};
use crate::workflows::WorkflowDef;

type WsSink = SplitSink<WebSocketStream<TcpStream>, Message>;

const CHANNEL_BUF: usize = 64;

enum Command {
    ListWorkflows {
        reply: oneshot::Sender<Response>,
    },
    ListSessions {
        reply: oneshot::Sender<Response>,
    },
    StartSession {
        workflow: WorkflowId,
        reply: oneshot::Sender<Response>,
    },
    StopSession {
        session: SessionId,
        reply: oneshot::Sender<Response>,
    },
    OpenSession {
        session: SessionId,
        reply: oneshot::Sender<Response>,
    },
}

/// Cheap-clonable handle. Holds the slug this project is bound to,
/// the issuer pubkey for inbound token verification, an mpsc sender
/// to the supervisor, and the broadcast sender for event fan-out.
#[derive(Clone)]
pub struct AppState {
    pub slug: Slug,
    pub issuer: VerifyingKey,
    /// Project-owned signing key. The supervisor passes this to
    /// [`SessionHandle::mint_token`] when handling `OpenSession`.
    pub signing: SigningKey,
    commands: mpsc::Sender<Command>,
    events: broadcast::Sender<Event>,
}

/// Owns the supervisor task. Holds the cheap-cloneable [`AppState`]
/// handle, the supervisor's [`JoinHandle`], and a oneshot shutdown
/// signal. Drop alone leaks the task; call [`Supervisor::shutdown`]
/// for explicit teardown.
pub struct Supervisor {
    pub state: AppState,
    pub join: JoinHandle<()>,
    pub shutdown: oneshot::Sender<()>,
}

/// Static config the supervisor task carries. Pulled out so the spawn
/// argument list stays manageable, and so the supervisor can pass the
/// same paths into every workflow subprocess without re-reading env.
pub struct SpawnConfig {
    pub workflows: Vec<WorkflowDef>,
    /// Global `.lutin/` root (read-only to workflows; for two-tier
    /// settings/persona resolution).
    pub global_config_dir: PathBuf,
    /// Per-project `.lutin/` root.
    pub project_config_dir: PathBuf,
}

impl Supervisor {
    /// Construct an [`AppState`] and spawn the supervisor task.
    /// `workflows` is the immutable set discovered at startup; it
    /// gates `StartSession` and is returned by `ListWorkflows`. The
    /// project supervisor itself never touches LLMs — it spawns
    /// workflow binaries that own everything substantive.
    pub fn spawn(
        slug: Slug,
        issuer: VerifyingKey,
        signing: SigningKey,
        config: SpawnConfig,
    ) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel(CHANNEL_BUF);
        let (ev_tx, _) = broadcast::channel(CHANNEL_BUF);
        let (sd_tx, sd_rx) = oneshot::channel();
        let ctx = SupervisorCtx {
            slug: slug.clone(),
            signing: signing.clone(),
            workflows: config.workflows,
            global_config_dir: config.global_config_dir,
            project_config_dir: config.project_config_dir,
        };
        let join = tokio::spawn(supervisor(cmd_rx, ev_tx.clone(), sd_rx, ctx));
        let state = AppState {
            slug,
            issuer,
            signing,
            commands: cmd_tx,
            events: ev_tx,
        };
        Self {
            state,
            join,
            shutdown: sd_tx,
        }
    }

    /// Signal the supervisor to exit and await the task.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.join.await;
    }
}

impl AppState {
    async fn dispatch(&self, req: Request) -> Response {
        let (reply, rx) = oneshot::channel();
        let cmd = match req {
            Request::ListWorkflows => Command::ListWorkflows { reply },
            Request::ListSessions => Command::ListSessions { reply },
            Request::StartSession { workflow } => Command::StartSession { workflow, reply },
            Request::StopSession { session } => Command::StopSession { session, reply },
            Request::OpenSession { session } => Command::OpenSession { session, reply },
        };
        if self.commands.send(cmd).await.is_err() {
            return Response::Err(ApiError::SupervisorStopped);
        }
        match rx.await {
            Ok(r) => r,
            Err(_) => Response::Err(ApiError::SupervisorDroppedReply),
        }
    }
}

struct SupervisorCtx {
    slug: Slug,
    signing: SigningKey,
    workflows: Vec<WorkflowDef>,
    global_config_dir: PathBuf,
    project_config_dir: PathBuf,
}

struct RunningSession {
    info: SessionInfo,
    handle: SessionHandle,
}

async fn supervisor(
    mut rx: mpsc::Receiver<Command>,
    events: broadcast::Sender<Event>,
    mut shutdown: oneshot::Receiver<()>,
    ctx: SupervisorCtx,
) {
    let mut sessions: Vec<RunningSession> = Vec::new();
    let workflow_infos: Vec<_> = ctx.workflows.iter().map(|w| w.info.clone()).collect();
    loop {
        let cmd = tokio::select! {
            biased;
            _ = &mut shutdown => break,
            cmd = rx.recv() => match cmd {
                Some(c) => c,
                None => break,
            },
        };
        match cmd {
            Command::ListWorkflows { reply } => {
                let _ = reply.send(Response::Ok(ResponseOk::Workflows(workflow_infos.clone())));
            }
            Command::ListSessions { reply } => {
                let infos: Vec<_> = sessions.iter().map(|s| s.info.clone()).collect();
                let _ = reply.send(Response::Ok(ResponseOk::Sessions(infos)));
            }
            Command::StartSession { workflow, reply } => {
                let Some(def) = ctx.workflows.iter().find(|w| w.info.id == workflow) else {
                    let _ = reply.send(Response::Err(ApiError::WorkflowNotFound(workflow)));
                    continue;
                };
                let id = match mint_session_id() {
                    Ok(id) => id,
                    Err(e) => {
                        let _ = reply.send(Response::Err(ApiError::Internal(format!("rng: {e}"))));
                        continue;
                    }
                };
                match run_build(def, &id, &events).await {
                    Ok(()) => {}
                    Err(e) => {
                        let _ = reply.send(Response::Err(e));
                        continue;
                    }
                }
                let project_pubkey_b64 = pubkey_to_string(&ctx.signing.verifying_key());
                let session_dir = default_session_dir(&ctx.project_config_dir, &id);
                let handle = match session::spawn_session(
                    &ctx.slug,
                    &workflow,
                    &id,
                    def,
                    &project_pubkey_b64,
                    &ctx.global_config_dir,
                    &ctx.project_config_dir,
                    &session_dir,
                )
                .await
                {
                    Ok(h) => h,
                    Err(e) => {
                        let _ = reply.send(Response::Err(ApiError::Internal(format!(
                            "spawn session: {e}"
                        ))));
                        continue;
                    }
                };
                let info = SessionInfo {
                    id: id.clone(),
                    workflow,
                };
                sessions.push(RunningSession {
                    info: info.clone(),
                    handle,
                });
                let _ = reply.send(Response::Ok(ResponseOk::Started(info.clone())));
                let _ = events.send(Event::SessionStarted(info));
            }
            Command::StopSession { session, reply } => {
                let Some(idx) = sessions.iter().position(|s| s.info.id == session) else {
                    let _ = reply.send(Response::Err(ApiError::SessionNotFound(session)));
                    continue;
                };
                let entry = sessions.swap_remove(idx);
                // Detach teardown so the supervisor loop never blocks on
                // listener/runtime joins.
                tokio::spawn(async move { entry.handle.stop().await });
                let _ = reply.send(Response::Ok(ResponseOk::Stopped));
                let _ = events.send(Event::SessionEnded { id: session });
            }
            Command::OpenSession { session, reply } => {
                let Some(entry) = sessions.iter().find(|s| s.info.id == session) else {
                    let _ = reply.send(Response::Err(ApiError::SessionNotFound(session)));
                    continue;
                };
                let token = match entry.handle.mint_token(
                    &ctx.signing,
                    ctx.slug.clone(),
                    entry.info.workflow.clone(),
                    entry.info.id.clone(),
                ) {
                    Ok(t) => t,
                    Err(e) => {
                        let _ = reply.send(Response::Err(ApiError::Internal(format!(
                            "mint session token: {e}"
                        ))));
                        continue;
                    }
                };
                let _ = reply.send(Response::Ok(ResponseOk::Opened(SessionEndpoint {
                    addr: entry.handle.addr,
                    token,
                })));
            }
        }
    }
    // Drain any still-running sessions so listeners + runtime tasks
    // don't outlive the supervisor.
    for s in sessions.drain(..) {
        tokio::spawn(async move { s.handle.stop().await });
    }
}

/// Run the workflow's cargo build, owning the lifecycle broadcasts.
/// Warm path (binary fresh) emits no events. Cold path emits
/// `WorkflowBuildStarted`, then delegates to `build::run_cargo` which
/// streams `WorkflowBuildOutput`, then emits `WorkflowBuildFinished`
/// with a typed outcome. Non-zero cargo exits become
/// `ApiError::WorkflowBuildFailed` so the StartSession reply is
/// matchable rather than string-parsed.
async fn run_build(
    def: &workflows::WorkflowDef,
    session: &SessionId,
    events: &broadcast::Sender<Event>,
) -> Result<(), ApiError> {
    match build::is_fresh(def) {
        Ok(true) => return Ok(()),
        Ok(false) => {}
        Err(e) => return Err(ApiError::Internal(format!("freshness check: {e}"))),
    }
    let workflow = def.info.id.clone();
    let _ = events.send(Event::WorkflowBuildStarted {
        session: session.clone(),
        workflow: workflow.clone(),
    });
    let exit_code = match build::run_cargo(def, session, events).await {
        Ok(code) => code,
        Err(e) => {
            // Spawn-time IO failure (e.g. cargo missing). Still emit
            // Finished so a subscriber that saw Started gets closure.
            let _ = events.send(Event::WorkflowBuildFinished {
                session: session.clone(),
                workflow,
                outcome: BuildOutcome::Failed { exit_code: None },
            });
            return Err(ApiError::Internal(format!("cargo build: {e}")));
        }
    };
    let outcome = if exit_code == Some(0) {
        BuildOutcome::Success
    } else {
        BuildOutcome::Failed { exit_code }
    };
    let _ = events.send(Event::WorkflowBuildFinished {
        session: session.clone(),
        workflow,
        outcome: outcome.clone(),
    });
    match outcome {
        BuildOutcome::Success => Ok(()),
        BuildOutcome::Failed { exit_code } => Err(ApiError::WorkflowBuildFailed { exit_code }),
    }
}

/// 128-bit random session id, hex-encoded (32 chars). Unguessable,
/// collision-free across supervisor restarts, fits the SessionId
/// charset (`[a-z0-9]`).
fn mint_session_id() -> Result<SessionId, getrandom::Error> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)?;
    let s: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(SessionId::parse(s).expect("hex32 is valid SessionId"))
}

async fn send_nack(tx: &mut WsSink, reason: &str) -> anyhow::Result<()> {
    let nack = encode(&Frame::HelloAck(HandshakeResult::Rejected {
        reason: reason.to_string(),
    }))?;
    tx.send(Message::Binary(nack.into())).await?;
    Ok(())
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
            Scope::Project(slug) if slug == &state.slug => {}
            _ => {
                return send_nack(
                    &mut tx,
                    &format!("scope must be Project({})", state.slug),
                )
                .await;
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
                    let body = pp::encode(&e)?;
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
                        let req = pp::decode::<Request>(&body)?;
                        let resp = state.dispatch(req).await;
                        let body = pp::encode(&resp)?;
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
    use lutin_auth::{Claims, generate_keypair};

    #[tokio::test]
    async fn session_handle_mint_token_roundtrips_with_correct_scope() {
        let signing = generate_keypair().unwrap();
        let pubkey = signing.verifying_key();
        let slug = Slug::parse("demo").unwrap();
        let workflow = WorkflowId::parse("chat").unwrap();
        let session = SessionId::parse("abc123").unwrap();

        let handle = SessionHandle::for_test("127.0.0.1:1".parse().unwrap());
        let token = handle
            .mint_token(&signing, slug.clone(), workflow.clone(), session.clone())
            .unwrap();

        let Claims { scope, subject, .. } = verify(&token, &pubkey).unwrap();
        assert_eq!(subject.as_str(), "project-supervisor");
        match scope {
            Scope::WorkflowSession {
                project,
                workflow: w,
                session: s,
            } => {
                assert_eq!(project, slug);
                assert_eq!(w, workflow);
                assert_eq!(s, session);
            }
            other => panic!("unexpected scope: {other:?}"),
        }
    }
}
