//! Per-project tier-2 WebSocket worker (chrome-side, parallel to the
//! workflow's `Transport` bridge).
//!
//! Chrome needs to call project-tier requests (`StartSession`,
//! `OpenSession`, `ListSessions`) and observe project broadcasts
//! (session lifecycle, workflow build progress) without going through
//! the workflow's own bytes-pipe. We therefore open a second TCP
//! connection per opened project — one for the workflow UI, one for
//! chrome itself. PLAN: "One TCP connection per open project;
//! acceptable."
//!
//! Same handshake shape as `cp.rs`: Hello → HelloAck → Payload /
//! Broadcast pump. Reconnects with the same 750ms backoff.
//!
//! Multiple project workers all push into a single shared
//! `mpsc<ProjUpdate>` keyed by `slug`, so the App can drain one channel
//! and route by `update.slug`.
//!
//! Drop semantics: dropping the worker's `cmd_tx` (kept alongside the
//! abort handle in `App::ProjWorker`) ends `cmd_rx`; the worker exits.
//! Aborting the spawned task tears down the WS unconditionally.

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use futures_util::{SinkExt, StreamExt};
use lutin_ids::Slug;
use lutin_project_protocol::{
    self as proj, Event as ProjEvent, Request as ProjRequest, Response as ProjResponse,
};
use lutin_protocol::{Frame, HandshakeResult, PROTOCOL_VERSION, decode, encode};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use tracing::{debug, warn};

use crate::cp::RequestId;

const RECONNECT_DELAY: Duration = Duration::from_millis(750);

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// UI → project worker.
#[derive(Debug, Clone)]
pub enum ProjCommand {
    Send {
        request_id: RequestId,
        request: ProjRequest,
    },
}

/// Project worker → UI. `slug` lets a single shared channel multiplex
/// across all opened projects.
#[derive(Debug, Clone)]
pub struct ProjUpdate {
    pub slug: Slug,
    pub kind: ProjUpdateKind,
}

#[derive(Debug, Clone)]
pub enum ProjUpdateKind {
    Connected,
    Disconnected,
    HandshakeRejected(String),
    ConnectError(String),
    Response {
        request_id: RequestId,
        response: ProjResponse,
    },
    Broadcast(ProjEvent),
}

/// Send an update and trigger a UI repaint. Centralizes the
/// "send-then-repaint" pair so the pump can't accidentally emit an
/// update without waking the UI.
fn emit(
    evt_tx: &mpsc::UnboundedSender<ProjUpdate>,
    repaint: &(dyn Fn() + Send + Sync),
    update: ProjUpdate,
) -> anyhow::Result<()> {
    evt_tx
        .send(update)
        .map_err(|_| anyhow!("ui receiver gone"))?;
    repaint();
    Ok(())
}

async fn connect_and_pump(
    addr: SocketAddr,
    token: &str,
    slug: &Slug,
    cmd_rx: &mut mpsc::UnboundedReceiver<ProjCommand>,
    evt_tx: &mpsc::UnboundedSender<ProjUpdate>,
    repaint: &(dyn Fn() + Send + Sync),
) -> anyhow::Result<()> {
    let url = format!("ws://{addr}");
    let (mut ws, _) = connect_async(&url)
        .await
        .with_context(|| format!("connect {url}"))?;

    let hello = encode(&Frame::Hello {
        protocol_version: PROTOCOL_VERSION,
        token: token.to_string(),
    })?;
    ws.send(Message::Binary(hello.into())).await?;

    let ack = ws
        .next()
        .await
        .ok_or_else(|| anyhow!("server closed before HelloAck"))??;
    let bytes = match ack {
        Message::Binary(b) => b,
        other => return Err(anyhow!("expected binary HelloAck, got {other:?}")),
    };
    match decode(&bytes)? {
        Frame::HelloAck(HandshakeResult::Accepted) => {}
        Frame::HelloAck(HandshakeResult::Rejected { reason }) => {
            emit(
                evt_tx,
                repaint,
                ProjUpdate {
                    slug: slug.clone(),
                    kind: ProjUpdateKind::HandshakeRejected(reason),
                },
            )?;
            return Ok(());
        }
        other => return Err(anyhow!("expected HelloAck, got {other:?}")),
    }
    emit(
        evt_tx,
        repaint,
        ProjUpdate {
            slug: slug.clone(),
            kind: ProjUpdateKind::Connected,
        },
    )?;

    pump(&mut ws, slug, cmd_rx, evt_tx, repaint).await
}

async fn pump(
    ws: &mut Ws,
    slug: &Slug,
    cmd_rx: &mut mpsc::UnboundedReceiver<ProjCommand>,
    evt_tx: &mpsc::UnboundedSender<ProjUpdate>,
    repaint: &(dyn Fn() + Send + Sync),
) -> anyhow::Result<()> {
    loop {
        tokio::select! {
            biased;

            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { return Ok(()); };
                match cmd {
                    ProjCommand::Send { request_id, request } => {
                        let body = proj::encode(&request)?;
                        let frame = encode(&Frame::Payload { request_id: request_id.0, body })?;
                        ws.send(Message::Binary(frame.into())).await?;
                    }
                }
            }

            msg = ws.next() => {
                let Some(msg) = msg else { return Ok(()); };
                let bytes = match msg? {
                    Message::Binary(b) => b,
                    Message::Close(_) => return Ok(()),
                    Message::Ping(p) => {
                        ws.send(Message::Pong(p)).await?;
                        continue;
                    }
                    _ => continue,
                };
                match decode(&bytes)? {
                    Frame::Payload { request_id, body } => {
                        let resp = proj::decode::<ProjResponse>(&body)?;
                        emit(evt_tx, repaint, ProjUpdate {
                            slug: slug.clone(),
                            kind: ProjUpdateKind::Response {
                                request_id: RequestId(request_id),
                                response: resp,
                            },
                        })?;
                    }
                    Frame::Broadcast { body } => {
                        let ev = proj::decode::<ProjEvent>(&body)?;
                        emit(evt_tx, repaint, ProjUpdate {
                            slug: slug.clone(),
                            kind: ProjUpdateKind::Broadcast(ev),
                        })?;
                    }
                    Frame::Pong { .. } => {}
                    Frame::Close { .. } => return Ok(()),
                    other => warn!(?other, %slug, "unexpected frame from project tier"),
                }
            }
        }
    }
}

/// Connect-and-reconnect loop for a single project. Exits when the App
/// drops `cmd_rx` (i.e. the project entry is removed). `repaint`
/// triggers a UI repaint after every emitted update.
pub async fn run(
    slug: Slug,
    addr: SocketAddr,
    token: String,
    mut cmd_rx: mpsc::UnboundedReceiver<ProjCommand>,
    evt_tx: mpsc::UnboundedSender<ProjUpdate>,
    repaint: impl Fn() + Send + Sync + 'static,
) {
    let repaint: &(dyn Fn() + Send + Sync) = &repaint;
    loop {
        match connect_and_pump(addr, &token, &slug, &mut cmd_rx, &evt_tx, repaint).await {
            Ok(()) => {
                let _ = emit(&evt_tx, repaint, ProjUpdate {
                    slug: slug.clone(),
                    kind: ProjUpdateKind::Disconnected,
                });
            }
            Err(e) => {
                warn!(error = %e, %slug, "project worker connection error");
                let _ = emit(&evt_tx, repaint, ProjUpdate {
                    slug: slug.clone(),
                    kind: ProjUpdateKind::ConnectError(e.to_string()),
                });
            }
        }
        if cmd_rx.is_closed() {
            debug!(%slug, "App dropped project command sender; worker exiting");
            return;
        }
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}
