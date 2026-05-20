//! ComfyUI HTTP client for the image workflow.
//!
//! Mirrors the discovery script in `workflows/image/scripts/smoketest.py`.
//! POSTs the FLUX-schnell graph, polls `/history` for completion, then
//! fetches the produced image bytes via `/view`. The WebSocket bridge
//! (Slice 4) runs as a separate task and translates ComfyUI's `/ws`
//! events into `ImageEvent` broadcasts; `/history` polling stays as a
//! fallback so jobs survive socket flaps.

use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

use image_workflow::{
    ImageEvent, ImageSettings, MODEL_CYBERREALISTIC_PONY, MODEL_FLUX2_DEV, MODEL_FLUX_SCHNELL,
};

/// FLUX schnell ships as a single fused checkpoint — `CheckpointLoaderSimple`
/// resolves model+CLIP+VAE in one node.
const FLUX_SCHNELL_CHECKPOINT: &str = "flux1-schnell-fp8.safetensors";
/// FLUX.2 dev splits the loader: UNet, text encoder, VAE.
/// Unlike FLUX.1, FLUX.2 uses a single Mistral 3 Small encoder rather
/// than the T5+CLIP-L pair, and ComfyUI exposes it through the
/// (single-slot) `CLIPLoader` with `type="flux2"` — not
/// `DualCLIPLoader`. Filenames match the official FLUX.2 dev release
/// shipped through ComfyUI's model manager; if a user installs
/// custom-named copies, `/prompt` returns a `value_not_in_list`
/// validation error naming the expected file and we surface it
/// verbatim.
const FLUX2_DEV_UNET: &str = "flux2_dev_fp8mixed.safetensors";
const FLUX2_DEV_CLIP: &str = "mistral_3_small_flux2_fp8.safetensors";
const FLUX2_DEV_VAE: &str = "flux2-vae.safetensors";
/// CyberRealistic Pony v14 — an SDXL/Pony-Diffusion derivative shipped
/// as a single fused checkpoint (model+CLIP+VAE), same loader shape as
/// FLUX schnell but SDXL latents and a conventional CFG scale.
const CYBERREALISTIC_PONY_CHECKPOINT: &str = "cyberrealisticPony_v140.safetensors";
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const POLL_TIMEOUT: Duration = Duration::from_secs(300);
const WS_RECONNECT_MIN: Duration = Duration::from_millis(500);
const WS_RECONNECT_MAX: Duration = Duration::from_secs(15);

/// One generated image's bytes plus its content type.
pub struct FetchedImage {
    pub mime: String,
    pub bytes: Vec<u8>,
}

/// Failure modes the engine cares to distinguish so it can map them
/// onto distinct `ImageError` variants on the wire.
pub enum ComfyError {
    /// Could not reach ComfyUI at all.
    Unreachable(String),
    /// Reached it, but ComfyUI reported the prompt as failed.
    Execution(String),
    /// Anything else (transport, JSON shape, …). Surfaces as Internal
    /// on the wire — these are bugs we want to see.
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for ComfyError {
    fn from(e: anyhow::Error) -> Self {
        ComfyError::Internal(e)
    }
}

/// All graph-shaping inputs passed through `queue_prompt`. Bundled
/// into a struct because `queue_prompt` already takes a long arg list
/// and Slice 5 added more (negative prompt, count, steps, cfg).
pub struct QueueParams<'a> {
    pub prompt: &'a str,
    pub negative_prompt: &'a str,
    pub seed: u64,
    pub width: u32,
    pub height: u32,
    pub batch_size: u32,
    pub steps: u32,
    /// KSampler `cfg` for schnell; FluxGuidance `guidance` for flux2-dev
    /// (the builder routes it). See `GenerateParams::cfg`.
    pub cfg: f32,
    pub model_id: &'a str,
}

/// Cheap reachability probe. Returns `Ok(())` only if ComfyUI's
/// `/system_stats` responds successfully.
pub async fn health_check(client: &reqwest::Client, base: &str) -> Result<(), String> {
    let url = format!("{base}/system_stats");
    match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => Ok(()),
        Ok(r) => Err(format!("{}: HTTP {}", base, r.status())),
        Err(e) => Err(format!("{base}: {e}")),
    }
}

/// Reachability probe + prompt enqueue. Returns the ComfyUI `prompt_id`
/// so the caller can broadcast `JobQueued` and bind WS progress events.
pub async fn queue_prompt(
    client: &reqwest::Client,
    base: &str,
    client_id: &str,
    params: &QueueParams<'_>,
) -> Result<String, ComfyError> {
    health_check(client, base).await.map_err(ComfyError::Unreachable)?;
    let graph = match params.model_id {
        MODEL_FLUX_SCHNELL => build_graph_flux_schnell(params),
        MODEL_FLUX2_DEV => build_graph_flux2_dev(params),
        MODEL_CYBERREALISTIC_PONY => build_graph_cyberrealistic_pony(params),
        other => {
            return Err(ComfyError::Execution(format!(
                "unknown model_id: {other}"
            )));
        }
    };
    queue(client, base, client_id, &graph).await
}

/// Poll `/history` until the prompt completes, then fetch every output
/// image's bytes in declared order.
pub async fn await_images(
    client: &reqwest::Client,
    base: &str,
    prompt_id: &str,
) -> Result<Vec<FetchedImage>, ComfyError> {
    let entry = wait_for(client, base, prompt_id).await?;
    let metas = collect_image_metas(&entry)?;
    if metas.is_empty() {
        return Err(ComfyError::Internal(anyhow!("no images in outputs")));
    }
    let mut out = Vec::with_capacity(metas.len());
    for m in &metas {
        let (bytes, mime) = fetch(client, base, m).await?;
        out.push(FetchedImage { mime, bytes });
    }
    Ok(out)
}

/// FLUX schnell graph: single fused checkpoint, KSampler at user cfg
/// (typically 1.0 for schnell), no FluxGuidance node.
fn build_graph_flux_schnell(p: &QueueParams<'_>) -> Value {
    json!({
        "3": {
            "class_type": "KSampler",
            "inputs": {
                "seed": p.seed,
                "steps": p.steps,
                "cfg": p.cfg,
                "sampler_name": "euler",
                "scheduler": "simple",
                "denoise": 1.0,
                "model": ["4", 0],
                "positive": ["6", 0],
                "negative": ["7", 0],
                "latent_image": ["5", 0],
            }
        },
        "4": {
            "class_type": "CheckpointLoaderSimple",
            "inputs": {"ckpt_name": FLUX_SCHNELL_CHECKPOINT}
        },
        "5": {
            "class_type": "EmptySD3LatentImage",
            "inputs": {"width": p.width, "height": p.height, "batch_size": p.batch_size}
        },
        "6": {
            "class_type": "CLIPTextEncode",
            "inputs": {"text": p.prompt, "clip": ["4", 1]}
        },
        "7": {
            "class_type": "CLIPTextEncode",
            "inputs": {"text": p.negative_prompt, "clip": ["4", 1]}
        },
        "8": {
            "class_type": "VAEDecode",
            "inputs": {"samples": ["3", 0], "vae": ["4", 2]}
        },
        "9": {
            "class_type": "SaveImage",
            "inputs": {"images": ["8", 0], "filename_prefix": "lutin"}
        }
    })
}

/// FLUX.2 dev graph: split UNet/CLIP/VAE loaders, FluxGuidance on the
/// positive conditioning (carrying the user's `cfg` as the actual
/// guidance), KSampler `cfg` forced to 1.0. Negative prompt still
/// passes through CLIP because FLUX expects a `negative` connection
/// even when guidance does the heavy lifting.
fn build_graph_flux2_dev(p: &QueueParams<'_>) -> Value {
    json!({
        "3": {
            "class_type": "KSampler",
            "inputs": {
                "seed": p.seed,
                "steps": p.steps,
                "cfg": 1.0,
                "sampler_name": "euler",
                "scheduler": "simple",
                "denoise": 1.0,
                "model": ["10", 0],
                "positive": ["13", 0],
                "negative": ["7", 0],
                "latent_image": ["5", 0],
            }
        },
        "5": {
            "class_type": "EmptySD3LatentImage",
            "inputs": {"width": p.width, "height": p.height, "batch_size": p.batch_size}
        },
        "6": {
            "class_type": "CLIPTextEncode",
            "inputs": {"text": p.prompt, "clip": ["11", 0]}
        },
        "7": {
            "class_type": "CLIPTextEncode",
            "inputs": {"text": p.negative_prompt, "clip": ["11", 0]}
        },
        "8": {
            "class_type": "VAEDecode",
            "inputs": {"samples": ["3", 0], "vae": ["12", 0]}
        },
        "9": {
            "class_type": "SaveImage",
            "inputs": {"images": ["8", 0], "filename_prefix": "lutin"}
        },
        "10": {
            "class_type": "UNETLoader",
            "inputs": {"unet_name": FLUX2_DEV_UNET, "weight_dtype": "default"}
        },
        "11": {
            "class_type": "CLIPLoader",
            "inputs": {
                "clip_name": FLUX2_DEV_CLIP,
                "type": "flux2"
            }
        },
        "12": {
            "class_type": "VAELoader",
            "inputs": {"vae_name": FLUX2_DEV_VAE}
        },
        "13": {
            "class_type": "FluxGuidance",
            "inputs": {"conditioning": ["6", 0], "guidance": p.cfg}
        }
    })
}

/// CyberRealistic Pony graph: standard SDXL pipeline with a fused
/// checkpoint, conventional CFG (~7), and `dpmpp_2m_sde` + `karras` —
/// the recipe the Pony Diffusion lineage was tuned for.
fn build_graph_cyberrealistic_pony(p: &QueueParams<'_>) -> Value {
    json!({
        "3": {
            "class_type": "KSampler",
            "inputs": {
                "seed": p.seed,
                "steps": p.steps,
                "cfg": p.cfg,
                "sampler_name": "dpmpp_2m_sde",
                "scheduler": "karras",
                "denoise": 1.0,
                "model": ["4", 0],
                "positive": ["6", 0],
                "negative": ["7", 0],
                "latent_image": ["5", 0],
            }
        },
        "4": {
            "class_type": "CheckpointLoaderSimple",
            "inputs": {"ckpt_name": CYBERREALISTIC_PONY_CHECKPOINT}
        },
        "5": {
            "class_type": "EmptyLatentImage",
            "inputs": {"width": p.width, "height": p.height, "batch_size": p.batch_size}
        },
        "6": {
            "class_type": "CLIPTextEncode",
            "inputs": {"text": p.prompt, "clip": ["10", 0]}
        },
        "7": {
            "class_type": "CLIPTextEncode",
            "inputs": {"text": p.negative_prompt, "clip": ["10", 0]}
        },
        "10": {
            "class_type": "CLIPSetLastLayer",
            "inputs": {"clip": ["4", 1], "stop_at_clip_layer": -2}
        },
        "8": {
            "class_type": "VAEDecode",
            "inputs": {"samples": ["3", 0], "vae": ["4", 2]}
        },
        "9": {
            "class_type": "SaveImage",
            "inputs": {"images": ["8", 0], "filename_prefix": "lutin"}
        }
    })
}

async fn queue(
    client: &reqwest::Client,
    base: &str,
    client_id: &str,
    graph: &Value,
) -> Result<String, ComfyError> {
    let body = json!({"prompt": graph, "client_id": client_id});
    let resp = client
        .post(format!("{base}/prompt"))
        .json(&body)
        .send()
        .await
        .map_err(|e| ComfyError::Unreachable(format!("POST /prompt: {e}")))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(ComfyError::Execution(format!(
            "POST /prompt: {status}: {text}"
        )));
    }
    let v: Value = resp
        .json()
        .await
        .context("POST /prompt: parse json")
        .map_err(ComfyError::Internal)?;
    v.get("prompt_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| ComfyError::Internal(anyhow!("/prompt missing prompt_id: {v}")))
}

async fn wait_for(
    client: &reqwest::Client,
    base: &str,
    prompt_id: &str,
) -> Result<Value, ComfyError> {
    let deadline = tokio::time::Instant::now() + POLL_TIMEOUT;
    loop {
        let url = format!("{base}/history/{prompt_id}");
        let resp = client
            .get(&url)
            .send()
            .await
            .map_err(|e| ComfyError::Unreachable(format!("GET {url}: {e}")))?;
        let v: Value = resp
            .json()
            .await
            .context("GET /history: parse json")
            .map_err(ComfyError::Internal)?;
        if let Some(entry) = v.get(prompt_id).cloned() {
            let status = entry.get("status").cloned().unwrap_or(Value::Null);
            let completed = status
                .get("completed")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let status_str = status
                .get("status_str")
                .and_then(Value::as_str)
                .unwrap_or("");
            if completed {
                return Ok(entry);
            }
            if status_str == "error" {
                return Err(ComfyError::Execution(extract_error_message(&status)));
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(ComfyError::Execution(format!(
                "prompt {prompt_id} did not finish within {:?}",
                POLL_TIMEOUT
            )));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Pull a human-friendly message out of a `/history` `status` block.
/// ComfyUI nests the real exception inside `messages[*].1.exception_message`,
/// so we walk the array and pick the first execution_error we see.
fn extract_error_message(status: &Value) -> String {
    let messages = status.get("messages").and_then(Value::as_array);
    if let Some(arr) = messages {
        for m in arr {
            let kind = m.get(0).and_then(Value::as_str);
            if kind == Some("execution_error") {
                if let Some(payload) = m.get(1) {
                    if let Some(msg) = payload.get("exception_message").and_then(Value::as_str) {
                        let node = payload
                            .get("node_type")
                            .and_then(Value::as_str)
                            .unwrap_or("?");
                        return format!("{node}: {msg}");
                    }
                }
            }
        }
    }
    "ComfyUI execution failed".to_string()
}

fn collect_image_metas(entry: &Value) -> Result<Vec<ImageMeta>, ComfyError> {
    let outputs = entry
        .get("outputs")
        .and_then(Value::as_object)
        .ok_or_else(|| ComfyError::Internal(anyhow!("history entry missing outputs")))?;
    let mut out = Vec::new();
    for (_node_id, node_out) in outputs {
        let Some(images) = node_out.get("images").and_then(Value::as_array) else {
            continue;
        };
        for img in images {
            let filename = img
                .get("filename")
                .and_then(Value::as_str)
                .ok_or_else(|| ComfyError::Internal(anyhow!("image missing filename")))?;
            out.push(ImageMeta {
                filename: filename.to_string(),
                subfolder: img
                    .get("subfolder")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                kind: img
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("output")
                    .to_string(),
            });
        }
    }
    Ok(out)
}

struct ImageMeta {
    filename: String,
    subfolder: String,
    kind: String,
}

async fn fetch(
    client: &reqwest::Client,
    base: &str,
    meta: &ImageMeta,
) -> Result<(Vec<u8>, String), ComfyError> {
    let url = format!(
        "{base}/view?filename={fn_}&subfolder={sub}&type={typ}",
        fn_ = urlenc(&meta.filename),
        sub = urlenc(&meta.subfolder),
        typ = urlenc(&meta.kind),
    );
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| ComfyError::Unreachable(format!("GET {url}: {e}")))?;
    let mime = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("image/png")
        .to_string();
    let bytes = resp
        .bytes()
        .await
        .context("GET /view: read body")
        .map_err(ComfyError::Internal)?
        .to_vec();
    Ok((bytes, mime))
}

/// Long-lived task: connect to ComfyUI's `/ws?clientId=<id>` and
/// translate progress / error events into `ImageEvent` broadcasts.
///
/// The ComfyUI URL is read from `settings` on every reconnect, so a
/// `SetSettings` call changing `comfyui_url` takes effect on the next
/// connect cycle (≤ `WS_RECONNECT_MAX`). Reconnects with exponential
/// backoff so a restart doesn't permanently silence progress; while
/// disconnected, the in-flight job's `await_images` keeps polling
/// `/history` so completion is never lost.
pub async fn ws_bridge(
    client_id: String,
    settings: std::sync::Arc<tokio::sync::RwLock<ImageSettings>>,
    events: broadcast::Sender<ImageEvent>,
) {
    let mut backoff = WS_RECONNECT_MIN;
    loop {
        let base = settings.read().await.comfyui_url.clone();
        let ws_base = http_to_ws(&base);
        let url = format!("{ws_base}/ws?clientId={}", urlenc(&client_id));
        match tokio_tungstenite::connect_async(&url).await {
            Ok((ws, _)) => {
                info!(%url, "comfy ws connected");
                backoff = WS_RECONNECT_MIN;
                let (mut tx, mut rx) = ws.split();
                while let Some(msg) = rx.next().await {
                    match msg {
                        Ok(Message::Text(text)) => {
                            if let Some(ev) = parse_ws_event(&text) {
                                let _ = events.send(ev);
                            }
                        }
                        Ok(Message::Ping(p)) => {
                            // Reply so ComfyUI doesn't drop us as
                            // unresponsive. Best-effort — if the send
                            // fails, the next read error will trigger
                            // the reconnect path.
                            let _ = tx.send(Message::Pong(p)).await;
                        }
                        Ok(Message::Close(_)) => {
                            warn!("comfy ws closed by peer");
                            break;
                        }
                        Ok(_) => {}
                        Err(e) => {
                            warn!(error = %e, "comfy ws read error");
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                debug!(error = %e, ?backoff, "comfy ws connect failed");
            }
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(WS_RECONNECT_MAX);
    }
}

/// Translate one ComfyUI WS message to an `ImageEvent`. Returns `None`
/// for messages we don't surface (status, executing, executed — the
/// completion side is owned by the `/history` poll).
fn parse_ws_event(text: &str) -> Option<ImageEvent> {
    let v: Value = serde_json::from_str(text).ok()?;
    let kind = v.get("type").and_then(Value::as_str)?;
    let data = v.get("data")?;
    let job_id = data
        .get("prompt_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if job_id.is_empty() {
        return None;
    }
    match kind {
        "progress" => {
            let step = data.get("value").and_then(Value::as_u64).unwrap_or(0) as u32;
            let total = data.get("max").and_then(Value::as_u64).unwrap_or(0) as u32;
            Some(ImageEvent::JobProgress {
                job_id,
                step,
                total,
            })
        }
        "execution_success" => Some(ImageEvent::JobDone { job_id }),
        "execution_error" => {
            let node = data
                .get("node_type")
                .and_then(Value::as_str)
                .unwrap_or("?");
            let msg = data
                .get("exception_message")
                .and_then(Value::as_str)
                .unwrap_or("execution failed");
            Some(ImageEvent::JobError {
                job_id,
                message: format!("{node}: {msg}"),
            })
        }
        _ => None,
    }
}

/// `http://x` → `ws://x`, `https://x` → `wss://x`. Anything else passes
/// through unchanged so a malformed setting at least produces a clear
/// connect-error rather than a silent no-op.
fn http_to_ws(base: &str) -> String {
    if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else {
        base.to_string()
    }
}

/// Tiny percent-encoder for filename / subfolder / type query params.
/// ComfyUI's filenames are usually plain ASCII, but subfolders may be
/// empty (encodes to empty) and we don't want to drag in a full URL
/// crate just for three fields.
fn urlenc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
