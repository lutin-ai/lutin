//! Image workflow engine binary.
//!
//! One subprocess per session. Same env handoff and WS handshake shape
//! as the chat workflow — see `workflows/chat/src/engine.rs` for the
//! full pattern. Slice 3 wires a single `Generate` request through to
//! a local ComfyUI instance and returns the resulting image bytes
//! inline (base64) on the protocol channel.

mod comfy;
mod settings;
mod store;
mod summary;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use futures_util::{SinkExt, StreamExt};
use image_workflow::{
    GenerateParams, GeneratedImage, ImageError, ImageEvent, ImageOk, ImageRequest,
    ImageResponse, ImageSettings, TranscriptEntry, TranscriptImage, TranscriptStatus,
    decode as img_decode, encode as img_encode,
};
use lutin_auth::{Scope, SessionId, Slug, VerifyingKey, WorkflowId, pubkey_from_str, verify};
use lutin_protocol::{Frame, HandshakeResult, PROTOCOL_VERSION, decode, encode};
use rand::RngCore;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{RwLock, broadcast};
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};

use crate::comfy::{ComfyError, FetchedImage, QueueParams};

struct Env {
    project: Slug,
    project_pubkey: VerifyingKey,
    workflow: WorkflowId,
    session: SessionId,
    addr: SocketAddr,
    handoff_path: PathBuf,
    /// `<project>/.lutin/`. Image-workflow settings live under
    /// `<project_config_dir>/image/lutin.image.toml`.
    project_config_dir: PathBuf,
    /// Per-session state dir, e.g. `<project>/.lutin/sessions/<id>/`.
    /// Holds `transcript.json`, `summary.json`, and `images/`.
    state_dir: PathBuf,
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
            project_config_dir: PathBuf::from(require_env("LUTIN_PROJECT_CONFIG_DIR")?),
            state_dir: PathBuf::from(require_env("LUTIN_SESSION_STATE_DIR")?),
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
    /// the WS bridge so ComfyUI delivers progress for prompts queued
    /// here to our connection.
    client_id: Arc<String>,
    /// Fan-out for progress events. The WS bridge feeds this; each
    /// connected workflow client subscribes and forwards into its own
    /// `Frame::Broadcast`.
    events: broadcast::Sender<ImageEvent>,
    /// Live settings, read on every Generate / WS reconnect. `RwLock`
    /// rather than `watch` because reads are frequent (every job) and
    /// writes are rare (a settings panel save).
    settings: Arc<RwLock<ImageSettings>>,
    /// `<project>/.lutin/` — the parent of the per-workflow settings
    /// dir. Held for the on-save persistence path.
    project_config_dir: PathBuf,
    /// Per-session dir; transcript + summary + images all live here.
    state_dir: PathBuf,
    /// Live transcript. Written under the lock on every turn (success
    /// or error) and replayed for `LoadTranscript`. Held in memory so
    /// the on-disk file is just a crash-recovery copy.
    transcript: Arc<RwLock<Vec<TranscriptEntry>>>,
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
    // 64 slots: comfortable headroom for one workflow client plus a
    // handful of stacked progress events. Lagged subscribers get
    // dropped (warn-logged) — same backpressure shape as chat.
    let (events, _) = broadcast::channel::<ImageEvent>(64);

    // Load persisted settings (defaults if missing). `RwLock` rather
    // than a one-shot read because `SetSettings` rewrites this at
    // runtime and the WS bridge / Generate paths read it back.
    let settings = match settings::load(&env.project_config_dir) {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "failed to load image settings; using defaults");
            ImageSettings::default()
        }
    };
    let settings = Arc::new(RwLock::new(settings));

    // Long-lived WS bridge to ComfyUI. Owns reconnection; the engine
    // doesn't restart it. Cloning is cheap; settings are read on every
    // reconnect cycle so a runtime URL change picks up automatically.
    tokio::spawn(comfy::ws_bridge(
        (*client_id).clone(),
        settings.clone(),
        events.clone(),
    ));

    // Load the persisted transcript (empty for first-run sessions)
    // and refresh summary.json so a resumed dormant session gets its
    // last_activity bumped even before any new turns happen.
    let transcript = match store::load(&env.state_dir) {
        Ok(t) => t,
        Err(e) => {
            warn!(error = %e, "failed to load transcript; starting empty");
            Vec::new()
        }
    };
    summary::write(&env.state_dir, &transcript);
    let transcript = Arc::new(RwLock::new(transcript));

    let state = AppState {
        project: env.project,
        workflow: env.workflow,
        session: env.session,
        issuer: env.project_pubkey,
        http,
        client_id,
        events,
        settings,
        project_config_dir: env.project_config_dir,
        state_dir: env.state_dir,
        transcript,
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

    let mut events = state.events.subscribe();

    loop {
        tokio::select! {
            biased;
            ev = events.recv() => match ev {
                Ok(e) => {
                    let body = img_encode(&e)?;
                    let frame = encode(&Frame::Broadcast { body })?;
                    if tx.send(Message::Binary(frame.into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(n, "client lagged ImageEvent broadcast; closing");
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
        }
    }
    Ok(())
}

async fn handle_request(state: &AppState, req: ImageRequest) -> ImageResponse {
    match req {
        ImageRequest::Generate(params) => handle_generate(state, params).await,
        ImageRequest::GetSettings => {
            let s = state.settings.read().await.clone();
            Ok(ImageOk::Settings(s))
        }
        ImageRequest::SetSettings(new) => {
            // Persist before swapping the live copy: a write failure
            // means the next process boot would silently drop the new
            // settings, which is a worse surprise than rejecting the
            // request now.
            if let Err(e) = settings::save(&state.project_config_dir, &new) {
                warn!(error = %e, "failed to save image settings");
                return Err(ImageError::Internal(format!("save settings: {e:#}")));
            }
            *state.settings.write().await = new;
            Ok(ImageOk::SettingsUpdated)
        }
        ImageRequest::HealthCheck => {
            let base = state.settings.read().await.comfyui_url.clone();
            match comfy::health_check(&state.http, &base).await {
                Ok(()) => Ok(ImageOk::Health {
                    reachable: true,
                    message: base,
                }),
                Err(msg) => Ok(ImageOk::Health {
                    reachable: false,
                    message: msg,
                }),
            }
        }
        ImageRequest::LoadTranscript => {
            let entries = state.transcript.read().await.clone();
            Ok(ImageOk::Transcript(entries))
        }
        ImageRequest::GetImage(image_id) => handle_get_image(state, &image_id).await,
    }
}

/// Resolve `image_id` against the session state dir, refusing any
/// path that escapes it. The well-formed shape is `images/<file>`;
/// anything with `..` or an absolute prefix is rejected before we
/// touch the filesystem.
async fn handle_get_image(state: &AppState, image_id: &str) -> ImageResponse {
    if image_id.is_empty()
        || image_id.contains("..")
        || image_id.starts_with('/')
        || image_id.contains('\\')
    {
        return Err(ImageError::Internal(format!("invalid image_id: {image_id}")));
    }
    let path = state.state_dir.join(image_id);
    let bytes = match tokio::fs::read(&path).await {
        Ok(b) => b,
        Err(e) => return Err(ImageError::Internal(format!("read {}: {e}", path.display()))),
    };
    let mime = mime_for(image_id);
    // We don't track per-image seed/ms after restore — the transcript
    // entry carries those. `GetImage` only needs to deliver bytes.
    Ok(ImageOk::Image(GeneratedImage {
        image_id: image_id.to_string(),
        mime,
        bytes_b64: B64.encode(&bytes),
        seed: 0,
        ms: 0,
    }))
}

fn mime_for(image_id: &str) -> String {
    let ext = image_id.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        _ => "application/octet-stream",
    }
    .to_string()
}

async fn handle_generate(state: &AppState, params: GenerateParams) -> ImageResponse {
    if params.count == 0 {
        return Err(ImageError::Internal("count must be >= 1".into()));
    }
    let seed = params.seed.unwrap_or_else(random_seed);
    let started = Instant::now();
    let started_at = chrono::Utc::now().to_rfc3339();
    let base = state.settings.read().await.comfyui_url.clone();
    info!(
        prompt_len = params.prompt.len(),
        seed,
        width = params.width,
        height = params.height,
        count = params.count,
        steps = params.steps,
        cfg = params.cfg,
        "generate start"
    );
    // Two-step: queue first so we have a `prompt_id` to bind progress
    // events to, then await completion. Between the two we broadcast
    // `JobQueued` so the UI can switch the pending turn from
    // "generating…" to a real progress bar before the first WS event
    // lands.
    let qp = QueueParams {
        prompt: &params.prompt,
        negative_prompt: &params.negative_prompt,
        seed,
        width: params.width,
        height: params.height,
        batch_size: params.count,
        steps: params.steps,
        cfg: params.cfg,
        model_id: &params.model_id,
    };
    let prompt_id =
        match comfy::queue_prompt(&state.http, &base, state.client_id.as_str(), &qp).await {
            Ok(id) => id,
            Err(e) => {
                let err = map_comfy_err(e, "queue");
                append_error_entry(state, &params, &started_at, &err).await;
                return Err(err);
            }
        };
    let _ = state.events.send(ImageEvent::JobQueued {
        job_id: prompt_id.clone(),
    });

    match comfy::await_images(&state.http, &base, &prompt_id).await {
        Ok(fetched) => {
            let ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
            info!(
                seed,
                ms,
                count = fetched.len(),
                %prompt_id,
                "generate ok"
            );
            // The WS bridge usually delivers `execution_success` first,
            // but emit our own `JobDone` too — that way progress UIs
            // work even if the WS is currently disconnected (the
            // `/history` poll covered the completion side).
            let _ = state.events.send(ImageEvent::JobDone {
                job_id: prompt_id,
            });
            // Write each image to disk before responding so a crash
            // mid-response doesn't leave the UI with a base64 blob it
            // can't re-fetch on next load.
            let mut images: Vec<GeneratedImage> = Vec::with_capacity(fetched.len());
            for (idx, FetchedImage { mime, bytes }) in fetched.into_iter().enumerate() {
                let image_id = match write_image(state, &started_at, seed, idx, &mime, &bytes) {
                    Ok(id) => id,
                    Err(e) => {
                        let err = ImageError::Internal(format!("save image: {e:#}"));
                        append_error_entry(state, &params, &started_at, &err).await;
                        return Err(err);
                    }
                };
                images.push(GeneratedImage {
                    image_id,
                    mime,
                    bytes_b64: B64.encode(&bytes),
                    seed,
                    ms,
                });
            }
            let entry = TranscriptEntry {
                prompt: params.prompt.clone(),
                negative_prompt: params.negative_prompt.clone(),
                width: params.width,
                height: params.height,
                steps: params.steps,
                cfg: params.cfg,
                model_id: params.model_id.clone(),
                started_at: started_at.clone(),
                status: TranscriptStatus::Done {
                    images: images
                        .iter()
                        .map(|i| TranscriptImage {
                            image_id: i.image_id.clone(),
                            mime: i.mime.clone(),
                            seed: i.seed,
                            ms: i.ms,
                        })
                        .collect(),
                },
            };
            persist_entry(state, entry).await;
            Ok(ImageOk::Images(images))
        }
        Err(e) => {
            let err = map_comfy_err(e, "await");
            let message = match &err {
                ImageError::ComfyUnreachable(r) => format!("comfy unreachable: {r}"),
                ImageError::Comfy(r) => r.clone(),
                ImageError::Internal(m) => m.clone(),
            };
            let _ = state.events.send(ImageEvent::JobError {
                job_id: prompt_id,
                message,
            });
            append_error_entry(state, &params, &started_at, &err).await;
            Err(err)
        }
    }
}

/// Write the image bytes under `<state_dir>/images/<ts>-<seed>-<idx>.<ext>`
/// and return its session-relative `image_id`. Creates the `images/`
/// dir on first call.
fn write_image(
    state: &AppState,
    started_at: &str,
    seed: u64,
    idx: usize,
    mime: &str,
    bytes: &[u8],
) -> Result<String> {
    let images_dir = state.state_dir.join("images");
    std::fs::create_dir_all(&images_dir)
        .with_context(|| format!("mkdir {}", images_dir.display()))?;
    let ext = match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        _ => "bin",
    };
    // RFC3339 has colons which are unfriendly in filenames on some
    // platforms; strip them along with `+`/`.` for a flat,
    // sort-friendly stem.
    let stamp: String = started_at
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    let filename = format!("{stamp}-{seed}-{idx}.{ext}");
    let path = images_dir.join(&filename);
    std::fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))?;
    Ok(format!("images/{filename}"))
}

async fn persist_entry(state: &AppState, entry: TranscriptEntry) {
    let mut entries = state.transcript.write().await;
    entries.push(entry);
    if let Err(e) = store::save(&state.state_dir, &entries) {
        warn!(error = %e, "failed to persist image transcript");
    }
    summary::write(&state.state_dir, &entries);
}

async fn append_error_entry(
    state: &AppState,
    params: &GenerateParams,
    started_at: &str,
    err: &ImageError,
) {
    let message = match err {
        ImageError::ComfyUnreachable(r) => format!("comfy unreachable: {r}"),
        ImageError::Comfy(r) => r.clone(),
        ImageError::Internal(m) => m.clone(),
    };
    let entry = TranscriptEntry {
        prompt: params.prompt.clone(),
        negative_prompt: params.negative_prompt.clone(),
        width: params.width,
        height: params.height,
        steps: params.steps,
        cfg: params.cfg,
        model_id: params.model_id.clone(),
        started_at: started_at.to_string(),
        status: TranscriptStatus::Error { message },
    };
    persist_entry(state, entry).await;
}

fn map_comfy_err(e: ComfyError, stage: &str) -> ImageError {
    match e {
        ComfyError::Unreachable(r) => {
            warn!(stage, reason = %r, "comfy unreachable");
            ImageError::ComfyUnreachable(r)
        }
        ComfyError::Execution(r) => {
            warn!(stage, reason = %r, "comfy execution error");
            ImageError::Comfy(r)
        }
        ComfyError::Internal(e) => {
            warn!(stage, error = %e, "comfy internal error");
            ImageError::Internal(format!("{e:#}"))
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
