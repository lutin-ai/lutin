//! Control-panel server. WS endpoint, CP-orchestrated session list,
//! request dispatch, broadcast fan-out. Holds the control-panel
//! signing key.

pub mod defaults;
mod registry;
pub mod sessions;
pub mod workflow_images;

use futures_util::{SinkExt, StreamExt};
use lutin_auth::{Scope, SigningKey, VerifyingKey, verify};
use lutin_control_protocol::{
    self as cp, ApiError, DisplayName, Event, ProjectInfo, Request, Response, ResponseOk,
    SessionId, Slug, WorkflowId,
};
use lutin_protocol::{Frame, HandshakeResult, PROTOCOL_VERSION, decode, encode};
use std::path::{Path, PathBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tracing::warn;

const CHANNEL_BUF: usize = 64;

/// Server-side project record. CP owns the per-project signing key
/// in-memory; `start_session` / `open_session` mint tokens against it
/// without re-reading disk.
#[derive(Debug, Clone)]
pub struct ProjectRecord {
    pub info: ProjectInfo,
    pub signing: SigningKey,
}

/// Where to find per-project state. Lives in the supervisor task.
#[derive(Clone)]
pub struct SpawnConfig {
    /// Parent dir of all per-project trees.
    pub projects_root: PathBuf,
    /// Global `.lutin/` directory.
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
    ListWorkflows {
        reply: oneshot::Sender<Response>,
    },
    ListSessions {
        slug: Slug,
        reply: oneshot::Sender<Response>,
    },
    StartSession {
        slug: Slug,
        workflow: WorkflowId,
        reply: oneshot::Sender<Response>,
    },
    StopSession {
        slug: Slug,
        session: SessionId,
        reply: oneshot::Sender<Response>,
    },
    OpenSession {
        slug: Slug,
        session: SessionId,
        reply: oneshot::Sender<Response>,
    },
}

#[derive(Clone)]
pub struct AppState {
    pub issuer: VerifyingKey,
    commands: mpsc::Sender<Command>,
    events: broadcast::Sender<Event>,
}

pub struct Supervisor {
    pub state: AppState,
    pub join: JoinHandle<()>,
    pub shutdown: oneshot::Sender<()>,
}

impl Supervisor {
    pub fn spawn(signing: SigningKey, config: SpawnConfig) -> Self {
        let issuer = signing.verifying_key();
        let (cmd_tx, cmd_rx) = mpsc::channel(CHANNEL_BUF);
        let (ev_tx, _) = broadcast::channel(CHANNEL_BUF);
        let (sd_tx, sd_rx) = oneshot::channel();
        let join = tokio::spawn(supervisor(cmd_rx, ev_tx.clone(), sd_rx, config));
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
            Request::ListWorkflows => Command::ListWorkflows { reply },
            Request::ListSessions { slug } => Command::ListSessions { slug, reply },
            Request::StartSession { slug, workflow } => Command::StartSession {
                slug,
                workflow,
                reply,
            },
            Request::StopSession { slug, session } => Command::StopSession {
                slug,
                session,
                reply,
            },
            Request::OpenSession { slug, session } => Command::OpenSession {
                slug,
                session,
                reply,
            },
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

async fn supervisor(
    mut rx: mpsc::Receiver<Command>,
    events: broadcast::Sender<Event>,
    mut shutdown: oneshot::Receiver<()>,
    config: SpawnConfig,
) {
    let mut projects: Vec<ProjectRecord> = match registry::load(&config.projects_root) {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "failed to load project registry; starting empty");
            Vec::new()
        }
    };
    let mut session_registry: sessions::SessionRegistry = Vec::new();

    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => break,
            cmd = rx.recv() => {
                match cmd {
                    Some(c) => handle_command(c, &mut projects, &mut session_registry, &config, &events).await,
                    None => break,
                }
            }
        }
    }

    sessions::stop_all(&mut session_registry).await;
}

async fn handle_command(
    cmd: Command,
    projects: &mut Vec<ProjectRecord>,
    session_registry: &mut sessions::SessionRegistry,
    config: &SpawnConfig,
    events: &broadcast::Sender<Event>,
) {
    match cmd {
        Command::ListProjects { reply } => {
            let infos: Vec<ProjectInfo> = projects.iter().map(|r| r.info.clone()).collect();
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
            let signing = match init_project_storage(&config.projects_root, &slug) {
                Ok(s) => s,
                Err(e) => {
                    let _ = reply.send(Response::Err(ApiError::Supervisor(format!(
                        "init project storage for {}: {e}",
                        slug.as_str()
                    ))));
                    return;
                }
            };
            let info = ProjectInfo { slug, display_name };
            projects.push(ProjectRecord {
                info: info.clone(),
                signing,
            });
            if let Err(e) = registry::save(&config.projects_root, projects) {
                warn!(error = %e, "failed to persist project registry after create");
            }
            let _ = reply.send(Response::Ok(ResponseOk::Created(info.clone())));
            let _ = events.send(Event::ProjectCreated(info));
        }
        Command::DeleteProject { slug, reply } => {
            let before = projects.len();
            projects.retain(|p| p.info.slug != slug);
            if projects.len() == before {
                let _ = reply.send(Response::Err(ApiError::NotFound(slug)));
                return;
            }
            sessions::stop_all_for_slug(session_registry, &slug).await;
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
        Command::ListWorkflows { reply } => {
            let workflows = sessions::list_workflows(&config.global_config_dir);
            let _ = reply.send(Response::Ok(ResponseOk::Workflows(workflows)));
        }
        Command::ListSessions { slug, reply } => {
            if !projects.iter().any(|p| p.info.slug == slug) {
                let _ = reply.send(Response::Err(ApiError::NotFound(slug)));
                return;
            }
            let infos = sessions::list_sessions(session_registry, &slug);
            let _ = reply.send(Response::Ok(ResponseOk::Sessions(infos)));
        }
        Command::StartSession {
            slug,
            workflow,
            reply,
        } => {
            let Some(record) = projects.iter().find(|p| p.info.slug == slug) else {
                let _ = reply.send(Response::Err(ApiError::NotFound(slug)));
                return;
            };
            match sessions::start_session(
                session_registry,
                &slug,
                &workflow,
                &record.signing,
                &config.projects_root,
                &config.global_config_dir,
            )
            .await
            {
                Ok((running_session, endpoint)) => {
                    let info = running_session.info.clone();
                    let _ = events.send(Event::SessionStarted {
                        slug: slug.clone(),
                        info: info.clone(),
                    });
                    let _ = reply.send(Response::Ok(ResponseOk::SessionStarted {
                        info,
                        endpoint,
                    }));
                }
                Err(e) => {
                    let _ = reply.send(Response::Err(e.into()));
                }
            }
        }
        Command::StopSession {
            slug,
            session,
            reply,
        } => {
            match sessions::stop_session(session_registry, &slug, &session).await {
                Ok(()) => {
                    let _ = events.send(Event::SessionEnded {
                        slug: slug.clone(),
                        session,
                    });
                    let _ = reply.send(Response::Ok(ResponseOk::SessionStopped));
                }
                Err(e) => {
                    let _ = reply.send(Response::Err(e.into()));
                }
            }
        }
        Command::OpenSession {
            slug,
            session,
            reply,
        } => {
            let Some(record) = projects.iter().find(|p| p.info.slug == slug) else {
                let _ = reply.send(Response::Err(ApiError::NotFound(slug)));
                return;
            };
            match sessions::open_session(session_registry, &slug, &session, &record.signing) {
                Ok(endpoint) => {
                    let _ = reply.send(Response::Ok(ResponseOk::SessionOpened(endpoint)));
                }
                Err(e) => {
                    let _ = reply.send(Response::Err(e.into()));
                }
            }
        }
    }
}

/// Eagerly create the per-project on-disk layout and mint the project's
/// signing keypair if absent. Returns the loaded key so CP can hold it
/// in memory rather than re-reading disk on every session op.
fn init_project_storage(projects_root: &Path, slug: &Slug) -> std::io::Result<SigningKey> {
    let lutin_dir = projects_root.join(slug.as_str()).join(".lutin");
    std::fs::create_dir_all(&lutin_dir)?;
    let keypair_path = lutin_dir.join("keypair");
    lutin_keypair::load_or_create_keypair(&keypair_path).map_err(std::io::Error::other)
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
