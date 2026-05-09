//! ComfyUI HTTP client for the image workflow.
//!
//! Mirrors the discovery script in `workflows/image/scripts/smoketest.py`.
//! POSTs the FLUX-schnell graph, polls `/history` for completion, then
//! fetches the produced image bytes via `/view`. WebSocket progress
//! streaming arrives in Slice 4.

use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};

const COMFY_URL: &str = "http://127.0.0.1:8188";
const CHECKPOINT: &str = "flux1-schnell-fp8.safetensors";
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const POLL_TIMEOUT: Duration = Duration::from_secs(300);

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

/// Top-level entry: queue a FLUX-schnell prompt, wait for it, return
/// the first image's bytes.
pub async fn generate(
    client: &reqwest::Client,
    client_id: &str,
    prompt: &str,
    seed: u64,
    width: u32,
    height: u32,
) -> Result<FetchedImage, ComfyError> {
    // Cheap reachability probe so the user gets a clear error rather
    // than a confusing "JSON parse failed" when ComfyUI is down.
    if let Err(e) = client
        .get(format!("{COMFY_URL}/system_stats"))
        .send()
        .await
    {
        return Err(ComfyError::Unreachable(format!("{COMFY_URL}: {e}")));
    }

    let graph = build_graph(prompt, seed, width, height);
    let prompt_id = queue(client, client_id, &graph).await?;
    let entry = wait_for(client, &prompt_id).await?;
    let metas = collect_image_metas(&entry)?;
    let first = metas
        .first()
        .ok_or_else(|| ComfyError::Internal(anyhow!("no images in outputs")))?;
    let (bytes, mime) = fetch(client, first).await?;
    Ok(FetchedImage { mime, bytes })
}

fn build_graph(prompt: &str, seed: u64, width: u32, height: u32) -> Value {
    json!({
        "3": {
            "class_type": "KSampler",
            "inputs": {
                "seed": seed,
                "steps": 4,
                "cfg": 1.0,
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
            "inputs": {"ckpt_name": CHECKPOINT}
        },
        "5": {
            "class_type": "EmptySD3LatentImage",
            "inputs": {"width": width, "height": height, "batch_size": 1}
        },
        "6": {
            "class_type": "CLIPTextEncode",
            "inputs": {"text": prompt, "clip": ["4", 1]}
        },
        "7": {
            "class_type": "CLIPTextEncode",
            "inputs": {"text": "", "clip": ["4", 1]}
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
    client_id: &str,
    graph: &Value,
) -> Result<String, ComfyError> {
    let body = json!({"prompt": graph, "client_id": client_id});
    let resp = client
        .post(format!("{COMFY_URL}/prompt"))
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

async fn wait_for(client: &reqwest::Client, prompt_id: &str) -> Result<Value, ComfyError> {
    let deadline = tokio::time::Instant::now() + POLL_TIMEOUT;
    loop {
        let url = format!("{COMFY_URL}/history/{prompt_id}");
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
    meta: &ImageMeta,
) -> Result<(Vec<u8>, String), ComfyError> {
    let url = format!(
        "{COMFY_URL}/view?filename={fn_}&subfolder={sub}&type={typ}",
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
