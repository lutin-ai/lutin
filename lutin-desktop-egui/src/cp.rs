//! Control-panel WebSocket client task.
//!
//! Owns one TCP connection, drives the Hello/HelloAck handshake, then
//! pumps requests and broadcasts between the UI thread and the wire.
//! Reconnects on close (and on handshake-rejection waits and retries).

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

/// Newtype around the on-the-wire `request_id`. The UI uses it to
/// correlate a `CpCommand::Send` with the eventual
/// `CpUpdate::Response`; wrapping prevents accidentally swapping it
/// with other `u64` ids floating around the chrome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RequestId(pub u64);

/// UI → worker. Caller assigns `request_id` so the UI can pair the
/// later `CpUpdate::Response` with whatever in-flight context it cares
/// about (e.g. which slug an `OpenProject` is for).
#[derive(Debug, Clone)]
pub enum CpCommand {
    Send {
        request_id: RequestId,
        request: Request,
    },
}

/// Worker → UI.
#[derive(Debug, Clone)]
pub enum CpUpdate {
    Connected,
    Disconnected,
    HandshakeRejected(String),
    ConnectError(String),
    /// Correlated response. `request_id` matches `RequestSent.id` from
    /// a prior `CpCommand::Send`.
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

#[derive(Debug)]
pub enum TokenError {
    Empty,
}

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TokenError::Empty => write!(f, "token must not be empty or whitespace"),
        }
    }
}

impl std::error::Error for TokenError {}

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
/// the UI before the user sees it. `Ok(false)` is a clean session end.
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
                .map_err(|_| anyhow!("ui receiver gone"))?;
            return Ok(true);
        }
        other => return Err(anyhow!("expected HelloAck, got {other:?}")),
    }
    evt_tx
        .send(CpUpdate::Connected)
        .map_err(|_| anyhow!("ui receiver gone"))?;

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
                            .map_err(|_| anyhow!("ui receiver gone"))?;
                    }
                    Frame::Broadcast { body } => {
                        let ev = cp::decode::<CpEvent>(&body)?;
                        evt_tx
                            .send(CpUpdate::Broadcast(ev))
                            .map_err(|_| anyhow!("ui receiver gone"))?;
                    }
                    Frame::Pong { .. } => {}
                    Frame::Close { .. } => return Ok(()),
                    other => warn!(?other, "unexpected frame from control-panel"),
                }
            }
        }
    }
}

/// Self-contained handle to the cp worker task. Owns the
/// command/event channels and the worker's `AbortHandle`, so the rest
/// of the chrome talks to one object instead of three loose pieces.
/// `reconnect` swaps the underlying worker without restarting the
/// process — used by the Settings view to apply connection changes.
/// Modeled after `EngineConnection` in `../lutin/desktop/src/connection.rs`.
pub struct CpClient {
    cmd_tx: mpsc::UnboundedSender<CpCommand>,
    evt_rx: mpsc::UnboundedReceiver<CpUpdate>,
    /// `None` when no usable config is currently set — chrome runs in
    /// the Settings view until the user adds one.
    worker: Option<tokio::task::AbortHandle>,
}

impl CpClient {
    /// Build a fresh client. When `cfg` is `None`, no worker is
    /// spawned and `send` returns `Err(Disconnected)` until the user
    /// reconnects via `reconnect`.
    pub fn connect(
        tokio: &tokio::runtime::Handle,
        ctx: &egui::Context,
        cfg: Option<CpConfig>,
    ) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<CpCommand>();
        let (evt_tx, evt_rx) = mpsc::unbounded_channel::<CpUpdate>();
        let worker = cfg.map(|cfg| spawn_worker(tokio, ctx, cfg, cmd_rx, evt_tx));
        Self { cmd_tx, evt_rx, worker }
    }

    /// Tear down the current worker (if any), drop any in-flight
    /// events from it, and respawn against `cfg`. Caller is
    /// responsible for clearing any state that was scoped to the
    /// previous control-panel (open projects, pending requests, etc.)
    /// before invoking — those are owned upstream.
    pub fn reconnect(
        &mut self,
        tokio: &tokio::runtime::Handle,
        ctx: &egui::Context,
        cfg: Option<CpConfig>,
    ) {
        if let Some(h) = self.worker.take() {
            h.abort();
        }
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<CpCommand>();
        let (evt_tx, evt_rx) = mpsc::unbounded_channel::<CpUpdate>();
        self.cmd_tx = cmd_tx;
        self.evt_rx = evt_rx;
        self.worker = cfg.map(|cfg| spawn_worker(tokio, ctx, cfg, cmd_rx, evt_tx));
    }

    /// True when a worker is running. False before `reconnect` has
    /// supplied a usable config (or after a previous `reconnect(None)`).
    pub fn has_worker(&self) -> bool {
        self.worker.is_some()
    }

    /// Send a command to the worker. Returns `Err` when the worker
    /// channel is closed (e.g. no worker spawned yet, or the task
    /// died and hasn't been respawned).
    pub fn send(&self, cmd: CpCommand) -> Result<(), CpCommand> {
        self.cmd_tx.send(cmd).map_err(|e| e.0)
    }

    /// Pull the next pending update, if any. Non-blocking.
    pub fn try_recv(&mut self) -> Option<CpUpdate> {
        self.evt_rx.try_recv().ok()
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
    ctx: &egui::Context,
    cfg: CpConfig,
    cmd_rx: mpsc::UnboundedReceiver<CpCommand>,
    evt_tx: mpsc::UnboundedSender<CpUpdate>,
) -> tokio::task::AbortHandle {
    let ctx = ctx.clone();
    let task = tokio.spawn(run(cfg, cmd_rx, evt_tx, move || ctx.request_repaint()));
    task.abort_handle()
}

/// Connect-and-reconnect loop. Exits when the UI drops `cmd_rx`.
/// `repaint` is called after every emitted update so egui wakes up
/// promptly even when no input events are pending.
pub async fn run(
    cfg: CpConfig,
    mut cmd_rx: mpsc::UnboundedReceiver<CpCommand>,
    raw_tx: mpsc::UnboundedSender<CpUpdate>,
    repaint: impl Fn() + Send + 'static,
) {
    // Wrap raw_tx so every send triggers a UI repaint without each
    // call site having to remember.
    let (evt_tx, mut evt_rx) = mpsc::unbounded_channel::<CpUpdate>();
    tokio::spawn(async move {
        while let Some(ev) = evt_rx.recv().await {
            if raw_tx.send(ev).is_err() {
                break;
            }
            repaint();
        }
    });

    loop {
        match connect_and_pump(&cfg, &mut cmd_rx, &evt_tx).await {
            Ok(false) => {
                let _ = evt_tx.send(CpUpdate::Disconnected);
            }
            Ok(true) => {
                // Terminal event already emitted (e.g. handshake
                // rejection). Don't append Disconnected — it would
                // wipe the reason from the UI before the user sees it.
            }
            Err(e) => {
                warn!(error = %e, "control-panel connection error");
                let _ = evt_tx.send(CpUpdate::ConnectError(e.to_string()));
            }
        }
        if cmd_rx.is_closed() {
            debug!("UI dropped command sender; cp worker exiting");
            return;
        }
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}
