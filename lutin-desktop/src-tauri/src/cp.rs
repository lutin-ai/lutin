//! Control-panel WebSocket client task.
//!
//! Owns one TCP connection, drives the Hello/HelloAck handshake, then
//! pumps requests and broadcasts between the chrome (Tauri Rust core)
//! and the wire. Reconnects on close (and on handshake-rejection waits
//! and retries).

use std::time::Duration;

use anyhow::{Context as _, anyhow};
use futures_util::{SinkExt, StreamExt};
use lutin_control_protocol::{self as cp, Event as CpEvent, Request, Response};
use lutin_protocol::{Frame, HandshakeResult, PROTOCOL_VERSION, decode, encode};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use tracing::{debug, info, warn};

const RECONNECT_DELAY: Duration = Duration::from_millis(750);

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Newtype around the on-the-wire `request_id`. Wrapping prevents
/// accidental swaps with other `u64` ids floating around the chrome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct RequestId(pub u64);

/// Chrome → worker. Caller assigns `request_id` so it can pair the
/// later `CpUpdate::Response` with whatever in-flight context it cares
/// about.
#[derive(Debug, Clone)]
pub enum CpCommand {
    Send {
        request_id: RequestId,
        request: Request,
    },
}

/// Worker → chrome.
#[derive(Debug, Clone)]
pub enum CpUpdate {
    Connected,
    Disconnected,
    HandshakeRejected(String),
    ConnectError(String),
    Response {
        request_id: RequestId,
        response: Response,
    },
    Broadcast(CpEvent),
}

/// Opaque bearer token used to authenticate to the control-panel.
/// `Debug` redacts; no `Display` is provided to avoid accidental leaks.
#[derive(Clone)]
pub struct Token(String);

#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error("token must not be empty or whitespace")]
    Empty,
}

impl Token {
    pub fn new(s: String) -> Result<Self, TokenError> {
        if s.trim().is_empty() {
            return Err(TokenError::Empty);
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Token(<redacted>)")
    }
}

#[derive(Clone, Debug)]
pub struct CpConfig {
    pub url: url::Url,
    pub token: Token,
}

/// `Ok(true)` means a terminal `CpUpdate` (currently `HandshakeRejected`)
/// was already emitted on `evt_tx` and the caller must not append a
/// `Disconnected` — doing so would overwrite the rejection reason in
/// the chrome before the user sees it. `Ok(false)` is a clean session end.
async fn connect_and_pump(
    cfg: &CpConfig,
    cmd_rx: &mut mpsc::UnboundedReceiver<CpCommand>,
    evt_tx: &mpsc::UnboundedSender<CpUpdate>,
) -> anyhow::Result<bool> {
    info!(url = %cfg.url, "control-panel: dialing");
    let (mut ws, _) = connect_async(cfg.url.as_str())
        .await
        .with_context(|| format!("connect {}", cfg.url))?;

    let hello = encode(&Frame::Hello {
        protocol_version: PROTOCOL_VERSION,
        token: cfg.token.as_str().to_string(),
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
            warn!(reason = %reason, "control-panel rejected handshake");
            evt_tx
                .send(CpUpdate::HandshakeRejected(reason))
                .map_err(|_| anyhow!("chrome receiver gone"))?;
            return Ok(true);
        }
        other => return Err(anyhow!("expected HelloAck, got {other:?}")),
    }
    evt_tx
        .send(CpUpdate::Connected)
        .map_err(|_| anyhow!("chrome receiver gone"))?;

    pump(&mut ws, cmd_rx, evt_tx).await.map(|()| false)
}

async fn pump(
    ws: &mut Ws,
    cmd_rx: &mut mpsc::UnboundedReceiver<CpCommand>,
    evt_tx: &mpsc::UnboundedSender<CpUpdate>,
) -> anyhow::Result<()> {
    loop {
        tokio::select! {
            biased;

            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { return Ok(()); };
                match cmd {
                    CpCommand::Send { request_id, request } => {
                        let body = cp::encode(&request)?;
                        // `Frame` carries a bare `u64`; unwrap the
                        // newtype only at the wire boundary.
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
                        let resp = cp::decode::<Response>(&body)?;
                        evt_tx
                            .send(CpUpdate::Response {
                                request_id: RequestId(request_id),
                                response: resp,
                            })
                            .map_err(|_| anyhow!("chrome receiver gone"))?;
                    }
                    Frame::Broadcast { body } => {
                        let ev = cp::decode::<CpEvent>(&body)?;
                        evt_tx
                            .send(CpUpdate::Broadcast(ev))
                            .map_err(|_| anyhow!("chrome receiver gone"))?;
                    }
                    Frame::Pong { .. } => {}
                    Frame::Close { .. } => return Ok(()),
                    other => warn!(?other, "unexpected frame from control-panel"),
                }
            }
        }
    }
}

/// Self-contained handle to the cp worker task. Owns the command sender
/// and the worker's `AbortHandle`. Updates flow out via the
/// `UnboundedReceiver<CpUpdate>` that `connect` returns separately —
/// the chrome layer drains it and dispatches to JS via Tauri events.
pub struct CpClient {
    cmd_tx: mpsc::UnboundedSender<CpCommand>,
    /// `None` when no usable config is currently set — chrome runs in
    /// the Settings view until the user adds one.
    worker: Option<tokio::task::AbortHandle>,
}

impl CpClient {
    /// Build a fresh client. `evt_tx` is the stable update sink — owned
    /// upstream so a single drainer task can survive reconnects. When
    /// `cfg` is `None`, no worker is spawned and `send` returns `Err`
    /// until the user reconnects.
    pub fn connect(
        tokio: &tokio::runtime::Handle,
        cfg: Option<CpConfig>,
        evt_tx: mpsc::UnboundedSender<CpUpdate>,
    ) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<CpCommand>();
        let worker = cfg.map(|cfg| spawn_worker(tokio, cfg, cmd_rx, evt_tx));
        Self { cmd_tx, worker }
    }

    /// Tear down the current worker (if any) and respawn against `cfg`.
    /// Caller is responsible for clearing per-CP state (open projects,
    /// pending requests) before invoking.
    pub fn reconnect(
        &mut self,
        tokio: &tokio::runtime::Handle,
        cfg: Option<CpConfig>,
        evt_tx: mpsc::UnboundedSender<CpUpdate>,
    ) {
        if let Some(h) = self.worker.take() {
            h.abort();
        }
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<CpCommand>();
        self.cmd_tx = cmd_tx;
        self.worker = cfg.map(|cfg| spawn_worker(tokio, cfg, cmd_rx, evt_tx));
    }

    /// Send a command to the worker. Returns `Err` when the worker
    /// channel is closed (no worker spawned yet, or the task died).
    pub fn send(&self, cmd: CpCommand) -> Result<(), CpCommand> {
        self.cmd_tx.send(cmd).map_err(|e| e.0)
    }
}

impl Drop for CpClient {
    fn drop(&mut self) {
        if let Some(h) = self.worker.take() {
            h.abort();
        }
    }
}

fn spawn_worker(
    tokio: &tokio::runtime::Handle,
    cfg: CpConfig,
    cmd_rx: mpsc::UnboundedReceiver<CpCommand>,
    evt_tx: mpsc::UnboundedSender<CpUpdate>,
) -> tokio::task::AbortHandle {
    let task = tokio.spawn(run(cfg, cmd_rx, evt_tx));
    task.abort_handle()
}

/// Connect-and-reconnect loop. Exits when the chrome drops `cmd_rx`.
pub async fn run(
    cfg: CpConfig,
    mut cmd_rx: mpsc::UnboundedReceiver<CpCommand>,
    evt_tx: mpsc::UnboundedSender<CpUpdate>,
) {
    loop {
        match connect_and_pump(&cfg, &mut cmd_rx, &evt_tx).await {
            Ok(false) => {
                let _ = evt_tx.send(CpUpdate::Disconnected);
            }
            Ok(true) => {
                // Terminal event already emitted (e.g. handshake
                // rejection). Don't append Disconnected.
            }
            Err(e) => {
                warn!(error = %e, "control-panel connection error");
                let _ = evt_tx.send(CpUpdate::ConnectError(e.to_string()));
            }
        }
        if cmd_rx.is_closed() {
            debug!("chrome dropped command sender; cp worker exiting");
            return;
        }
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}
