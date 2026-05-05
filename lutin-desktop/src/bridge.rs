//! Workflow `Transport` ↔ WebSocket bridge.
//!
//! Same shape at the project tier (tier-2) and the session tier
//! (tier-3): chrome owns the WS, runs Hello/HelloAck, then forwards
//! `Frame::Payload` / `Frame::Broadcast` bytes verbatim across an
//! `mpsc` pair the workflow's `Transport` is built on top of.
//!
//! Drop = teardown:
//!   * If the workflow drops its `Transport`, `out_rx` closes and the
//!     pump exits, dropping the WS.
//!   * If the WS closes, `in_tx` is dropped and the workflow's `recv`
//!     stream ends.
//!
//! Hello/HelloAck/Ping/Pong are handled here and never crossed into
//! the workflow; per the `lutin-workflow-ui` contract, the workflow
//! only ever sees full `Frame::Payload` / `Frame::Broadcast` envelopes.

use anyhow::{Context as _, anyhow};
use futures_util::{SinkExt, StreamExt};
use lutin_ids::Slug;
use lutin_protocol::{Frame, HandshakeResult, PROTOCOL_VERSION, decode, encode};
use lutin_workflow_ui::{AuthToken, Transport};
use std::net::SocketAddr;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use tracing::{debug, error, warn};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Chrome-side counterpart to `Transport`. Holds the receiver of bytes
/// the workflow wants sent (workflow → engine) and the sender of bytes
/// arriving from the engine (engine → workflow).
pub struct BridgeEndpoints {
    /// Bytes the workflow pushed onto `Transport::send`. We forward
    /// each one verbatim into a `Message::Binary` on the WS.
    out_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    /// Bytes we read off the WS (after filtering out Hello/Ping/Pong).
    /// The workflow reads these from `Transport::recv`.
    in_tx: mpsc::UnboundedSender<Vec<u8>>,
}

/// Build a `Transport` for the workflow + the chrome-side
/// `BridgeEndpoints` that feed it. The two halves are paired by mpsc
/// channels; dropping either end tears the pair down.
pub fn make_transport_pair() -> (Transport, BridgeEndpoints) {
    let (out_tx, out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (in_tx, in_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    (
        Transport {
            send: out_tx,
            recv: in_rx,
        },
        BridgeEndpoints { out_rx, in_tx },
    )
}

/// Connect to `addr` over WS, do the `Hello` handshake with `token`,
/// then pump `Frame::Payload`/`Frame::Broadcast` bytes between the WS
/// and `endpoints` until either side closes.
///
/// Returns on clean teardown; logs and returns on error. Callers spawn
/// this on a tokio runtime and forget — the `BridgeEndpoints` /
/// `Transport` channel closure is the supervisor.
pub async fn run_workflow_bridge(
    slug: Slug,
    addr: SocketAddr,
    token: AuthToken,
    endpoints: BridgeEndpoints,
) {
    let url = format!("ws://{addr}");
    if let Err(e) = connect_and_pump(&url, &token, endpoints).await {
        // Terminal: the workflow's `Transport` in `LoadedProject` is now
        // wired to a dead WS. C2 leaves it that way (no auto-reconnect);
        // surface as `error!` so it shows up in logs with slug context.
        error!(error = %e, %slug, %url, "workflow bridge terminated");
    }
}

async fn connect_and_pump(
    url: &str,
    token: &AuthToken,
    mut endpoints: BridgeEndpoints,
) -> anyhow::Result<()> {
    let (mut ws, _) = connect_async(url)
        .await
        .with_context(|| format!("connect {url}"))?;

    let hello = encode(&Frame::Hello {
        protocol_version: PROTOCOL_VERSION,
        token: token.as_str().to_string(),
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
            return Err(anyhow!("handshake rejected: {reason}"));
        }
        other => return Err(anyhow!("expected HelloAck, got {other:?}")),
    }

    pump(&mut ws, &mut endpoints).await
}

async fn pump(ws: &mut Ws, endpoints: &mut BridgeEndpoints) -> anyhow::Result<()> {
    loop {
        tokio::select! {
            biased;

            // Workflow → WS. Bytes are already a fully-encoded Frame
            // (per the `Transport` contract); forward unchanged.
            out = endpoints.out_rx.recv() => {
                let Some(bytes) = out else {
                    debug!("workflow dropped Transport; closing WS");
                    return Ok(());
                };
                ws.send(Message::Binary(bytes.into())).await?;
            }

            // WS → workflow.
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
                // Decode just enough to filter chrome-owned frames out.
                // Forward Payload/Broadcast bytes unchanged so the
                // workflow can decode them with its own codec.
                match decode(&bytes)? {
                    Frame::Payload { .. } | Frame::Broadcast { .. } => {
                        if endpoints.in_tx.send(bytes.to_vec()).is_err() {
                            debug!("workflow dropped Transport recv; closing WS");
                            return Ok(());
                        }
                    }
                    Frame::Ping { nonce } => {
                        let pong = encode(&Frame::Pong { nonce })?;
                        ws.send(Message::Binary(pong.into())).await?;
                    }
                    Frame::Pong { .. } => {}
                    Frame::Close { .. } => return Ok(()),
                    other => warn!(?other, "unexpected frame from workflow tier"),
                }
            }
        }
    }
}
