//! Image workflow engine binary.
//!
//! One subprocess per session. Same env handoff and WS handshake shape
//! as the chat workflow — see `workflows/chat/src/engine.rs` for the
//! full pattern. Slice 3 wires a single `Generate` request through to
//! a local ComfyUI instance and returns the resulting image bytes
//! inline (base64) on the protocol channel.

mod comfy;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use futures_util::{SinkExt, StreamExt};
use image_workflow::{
    GeneratedImage, ImageError, ImageOk, ImageRequest, ImageResponse,
    decode as img_decode, encode as img_encode,
};
use lutin_auth::{Scope, SessionId, Slug, VerifyingKey, WorkflowId, pubkey_from_str, verify};
use lutin_protocol::{Frame, HandshakeResult, PROTOCOL_VERSION, decode, encode};
use rand::RngCore;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};

use crate::comfy::{ComfyError, FetchedImage};

struct Env {
    project: Slug,
    project_pubkey: VerifyingKey,
    workflow: WorkflowId,
    session: SessionId,
    addr: SocketAddr,
    handoff_path: PathBuf,
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
            addr: require_env("LUTIN_WORKFLOW_ADDR")?
                .parse()
                .context("LUTIN_WORKFLOW_ADDR is not a valid socket addr")?,
            handoff_path: PathBuf::from(require_env("LUTIN_WORKFLOW_HANDOFF_PATH")?),
        })
    }
}

fn require_env(key: &str) -> Result<String> {
    std::env::var(key).map_err(|_| anyhow!("missing required env var {key}"))
}

#[derive(Clone)]
struct AppState {
    project: Slug,
    workflow: WorkflowId,
    session: SessionId,
    issuer: VerifyingKey,
    http: reqwest::Client,
    /// Stable per-process id used on every ComfyUI POST. Re-used by
    /// the WS connection in Slice 4 so progress events route here.
    client_id: Arc<String>,
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
    info!(%bound, session = %env.session, "image workflow listening");

    lutin_keypair::write_atomic(&env.handoff_path, format!("{bound}\n").as_bytes(), 0o600)
        .with_context(|| format!("write handoff {}", env.handoff_path.display()))?;

    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .context("build reqwest client")?;
    let client_id = Arc::new(uuid_v4());

    let state = AppState {
        project: env.project,
        workflow: env.workflow,
        session: env.session,
        issuer: env.project_pubkey,
        http,
        client_id,
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
        let nack = encode(&Frame::HelloAck(HandshakeResult::Rejected {
            reason: format!(
                "protocol version mismatch: server={PROTOCOL_VERSION} client={protocol_version}"
            ),
        }))?;
        tx.send(Message::Binary(nack.into())).await?;
        return Ok(());
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
                let nack = encode(&Frame::HelloAck(HandshakeResult::Rejected {
                    reason: "scope mismatch for this workflow session".to_string(),
                }))?;
                tx.send(Message::Binary(nack.into())).await?;
                return Ok(());
            }
        },
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

    while let Some(msg) = rx.next().await {
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
                let req: ImageRequest = match img_decode(&body) {
                    Ok(r) => r,
                    Err(e) => {
                        warn!(error = %e, "failed to decode ImageRequest");
                        let resp: ImageResponse =
                            Err(ImageError::Internal(format!("decode request: {e}")));
                        let body = img_encode(&resp)?;
                        let out = encode(&Frame::Payload { request_id, body })?;
                        tx.send(Message::Binary(out.into())).await?;
                        continue;
                    }
                };
                let resp = handle_request(&state, req).await;
                let body = img_encode(&resp)?;
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
    Ok(())
}

async fn handle_request(state: &AppState, req: ImageRequest) -> ImageResponse {
    match req {
        ImageRequest::Generate(params) => {
            let seed = params.seed.unwrap_or_else(random_seed);
            let started = Instant::now();
            info!(
                prompt_len = params.prompt.len(),
                seed,
                width = params.width,
                height = params.height,
                "generate start"
            );
            let result = comfy::generate(
                &state.http,
                state.client_id.as_str(),
                &params.prompt,
                seed,
                params.width,
                params.height,
            )
            .await;
            let ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
            match result {
                Ok(FetchedImage { mime, bytes }) => {
                    info!(seed, ms, bytes = bytes.len(), "generate ok");
                    Ok(ImageOk::Image(GeneratedImage {
                        mime,
                        bytes_b64: B64.encode(&bytes),
                        seed,
                        ms,
                    }))
                }
                Err(ComfyError::Unreachable(r)) => {
                    warn!(reason = %r, "comfy unreachable");
                    Err(ImageError::ComfyUnreachable(r))
                }
                Err(ComfyError::Execution(r)) => {
                    warn!(reason = %r, "comfy execution error");
                    Err(ImageError::Comfy(r))
                }
                Err(ComfyError::Internal(e)) => {
                    warn!(error = %e, "comfy internal error");
                    Err(ImageError::Internal(format!("{e:#}")))
                }
            }
        }
    }
}

/// Random u64 seed. The graph happens to take an i64-typed input but
/// Comfy accepts the full u64 range without complaint.
fn random_seed() -> u64 {
    let mut buf = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut buf);
    u64::from_le_bytes(buf)
}

/// UUIDv4 hex-string suitable for ComfyUI's `client_id` field. We
/// don't need full UUID semantics — a unique opaque id per process
/// is enough — so we synthesize one rather than pull in a UUID crate
/// just for this string.
fn uuid_v4() -> String {
    let mut buf = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut buf);
    // RFC 4122 §4.4: set version (4) and variant (10xx) bits.
    buf[6] = (buf[6] & 0x0f) | 0x40;
    buf[8] = (buf[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
        buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15],
    )
}
