//! Per-session WebSocket bridge between chrome and a workflow engine.
//!
//! Each running workflow session in chrome owns one `EngineBridge` —
//! a tokio task that holds the WS connection, runs Hello/HelloAck
//! once, then routes traffic on behalf of the iframe:
//!
//! * Iframe → engine: chrome wraps `body` in `Frame::Payload { id }`,
//!   allocates the id, and stashes a oneshot for the matching reply.
//! * Engine → iframe: chrome decodes incoming frames; `Frame::Payload`
//!   replies are matched by id and resolve the oneshot, while
//!   `Frame::Broadcast` bodies fan out to every subscriber `Channel`.
//!
//! Tokens never leave Rust — the iframe side sees opaque session ids
//! only. Per the plan: "the JS bridge sees opaque session ids, never
//! tokens." Chrome holds the token internally and feeds it into the
//! handshake when dialling.
//!
//! Lifecycle: a bridge lives until either (a) the WS closes, (b) the
//! handle is explicitly closed via `BridgeCmd::Close`, or (c) the
//! command channel is dropped (chrome shutdown). Drop tears down the
//! pump, which drops the WS.

use std::collections::HashMap;

use anyhow::{Context as _, anyhow};
use futures_util::{SinkExt, StreamExt};
use lutin_protocol::{Frame, HandshakeResult, PROTOCOL_VERSION, decode, encode};
use tauri::ipc::Channel;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use tracing::{debug, error, warn};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Frame `body` payload streamed to subscribers. Tauri's `Channel<T>`
/// requires `Clone + Serialize`; raw `Vec<u8>` satisfies both and
/// reaches the JS side as a numeric array.
pub type EngineBytes = Vec<u8>;

/// Commands sent into a running bridge task.
pub enum BridgeCmd {
    /// Send a request body to the engine. The bridge wraps it in
    /// `Frame::Payload`, allocates the id, and resolves `reply` with
    /// the body of the matching response.
    Request {
        body: Vec<u8>,
        reply: oneshot::Sender<Result<Vec<u8>, String>>,
    },
    /// Register a broadcast subscriber. Every `Frame::Broadcast` body
    /// after this point is sent on `channel`. Closes when the bridge
    /// task exits.
    Subscribe { channel: Channel<EngineBytes> },
    /// Initiate a clean teardown.
    Close,
}

/// Handle held by chrome's `AppState`. Cloning shares the same
/// underlying bridge task; dropping all clones tears it down.
#[derive(Clone)]
pub struct BridgeHandle {
    tx: mpsc::UnboundedSender<BridgeCmd>,
}

impl BridgeHandle {
    pub fn send(&self, cmd: BridgeCmd) -> Result<(), &'static str> {
        self.tx.send(cmd).map_err(|_| "bridge task gone")
    }
}

/// Connect, run the handshake, and spawn the pump on `tokio`. Returns
/// the handle once the handshake completes (so callers can surface
/// connect errors synchronously) plus a never-resolves task; the
/// caller is expected to drop the handle when the session ends.
pub async fn connect(
    tokio: &tokio::runtime::Handle,
    url: String,
    token: String,
) -> Result<BridgeHandle, String> {
    let (ws, _) = connect_async(&url)
        .await
        .with_context(|| format!("connect {url}"))
        .map_err(|e| format!("{e:#}"))?;
    let ws = handshake(ws, &token)
        .await
        .map_err(|e| format!("handshake: {e:#}"))?;

    let (tx, rx) = mpsc::unbounded_channel::<BridgeCmd>();
    tokio.spawn(async move {
        if let Err(e) = pump(ws, rx).await {
            error!(url = %url, error = %format!("{e:#}"), "engine bridge terminated");
        }
    });
    Ok(BridgeHandle { tx })
}

async fn handshake(mut ws: Ws, token: &str) -> anyhow::Result<Ws> {
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
        Frame::HelloAck(HandshakeResult::Accepted) => Ok(ws),
        Frame::HelloAck(HandshakeResult::Rejected { reason }) => {
            Err(anyhow!("rejected: {reason}"))
        }
        other => Err(anyhow!("expected HelloAck, got {other:?}")),
    }
}

async fn pump(
    mut ws: Ws,
    mut cmd_rx: mpsc::UnboundedReceiver<BridgeCmd>,
) -> anyhow::Result<()> {
    let mut next_id: u64 = 1;
    let mut pending: HashMap<u64, oneshot::Sender<Result<Vec<u8>, String>>> = HashMap::new();
    let mut subscribers: Vec<Channel<EngineBytes>> = Vec::new();

    loop {
        tokio::select! {
            biased;

            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else {
                    debug!("bridge command channel closed; tearing down");
                    return Ok(());
                };
                match cmd {
                    BridgeCmd::Request { body, reply } => {
                        let id = next_id;
                        next_id += 1;
                        pending.insert(id, reply);
                        let frame = encode(&Frame::Payload { request_id: id, body })?;
                        ws.send(Message::Binary(frame.into())).await?;
                    }
                    BridgeCmd::Subscribe { channel } => subscribers.push(channel),
                    BridgeCmd::Close => return Ok(()),
                }
            }

            msg = ws.next() => {
                let Some(msg) = msg else { return Ok(()); };
                let bytes = match msg? {
                    Message::Binary(b) => b,
                    Message::Close(_) => return Ok(()),
                    Message::Ping(p) => { ws.send(Message::Pong(p)).await?; continue; }
                    _ => continue,
                };
                match decode(&bytes)? {
                    Frame::Payload { request_id, body } => {
                        if let Some(tx) = pending.remove(&request_id) {
                            let _ = tx.send(Ok(body));
                        } else {
                            warn!(request_id, "engine reply for unknown request id");
                        }
                    }
                    Frame::Broadcast { body } => {
                        // Drop any subscriber whose JS side has gone
                        // away — `Channel::send` returns Err once the
                        // command future is dropped. Iterating with
                        // `retain` keeps the live ones in place.
                        subscribers.retain(|ch| ch.send(body.clone()).is_ok());
                    }
                    Frame::Ping { nonce } => {
                        let pong = encode(&Frame::Pong { nonce })?;
                        ws.send(Message::Binary(pong.into())).await?;
                    }
                    Frame::Pong { .. } => {}
                    Frame::Close { .. } => return Ok(()),
                    other => warn!(?other, "unexpected frame from engine"),
                }
            }
        }
    }
}
