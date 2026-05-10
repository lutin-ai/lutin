//! Image workflow protocol.
//!
//! TS bindings live in `packages/image-protocol`; variant indices and
//! field order here are the wire schema. Adding/reordering arms is a
//! breaking change — bump both sides together.

use serde::{Deserialize, Serialize};

/// Project-scoped image-workflow settings. Persisted as
/// `<project>/.lutin/image/lutin.image.toml`. Every field is required
/// on the wire (the engine substitutes defaults at load time so the
/// UI sees a fully-populated shape).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageSettings {
    /// e.g. `http://127.0.0.1:8188`. The engine talks HTTP at this
    /// base; the WS bridge derives `ws://<host>/ws` from it.
    pub comfyui_url: String,
    pub default_width: u32,
    pub default_height: u32,
    pub default_count: u32,
    pub default_steps: u32,
    pub default_cfg: f32,
    /// Which model graph to build by default. Must be one of the ids
    /// returned by `MODEL_IDS` (engine validates on `Generate`). The
    /// per-model `default_steps` / `default_cfg` differ wildly (schnell
    /// = 4 steps / cfg 1, flux2-dev = 28 steps / guidance 3.5), so the
    /// UI overrides those placeholders based on the selected model
    /// rather than reusing the workflow-wide defaults blindly.
    pub default_model_id: String,
}

/// Stable string ids for the graph templates we know how to build.
/// Wire format: any string the workflow understands. Adding a new
/// variant means a matching arm in `comfy::build_graph` and a UI
/// dropdown entry; nothing here changes the protocol shape.
pub const MODEL_FLUX_SCHNELL: &str = "flux-schnell";
pub const MODEL_FLUX2_DEV: &str = "flux2-dev";
pub const MODEL_IDS: &[&str] = &[MODEL_FLUX_SCHNELL, MODEL_FLUX2_DEV];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateParams {
    pub prompt: String,
    pub negative_prompt: String,
    /// `None` means the engine picks a fresh u64.
    pub seed: Option<u64>,
    pub width: u32,
    pub height: u32,
    /// 1..=N. Renders as a grid in the UI.
    pub count: u32,
    pub steps: u32,
    /// Repurposed across model variants: for `flux-schnell` this is
    /// the KSampler `cfg` (typically 1.0). For `flux2-dev` this is
    /// the `FluxGuidance.guidance` (typically ~3.5) — KSampler `cfg`
    /// is forced to 1.0 by the graph builder. Keeping a single field
    /// matches the user-visible "CFG / guidance" knob in the UI.
    pub cfg: f32,
    /// Which graph builder to invoke. See `MODEL_IDS`.
    pub model_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ImageRequest {
    Generate(GenerateParams),
    GetSettings,
    SetSettings(ImageSettings),
    /// Cheap reachability probe — POST `/system_stats` and report.
    HealthCheck,
    /// Replay this session's persisted transcript on first paint.
    /// Returns metadata + image refs only; bytes are fetched lazily
    /// via `GetImage` so a session with hundreds of images doesn't
    /// hold the whole gallery in memory at once.
    LoadTranscript,
    /// Resolve an `image_id` (relative path under the session state
    /// dir) to its on-disk bytes, base64-wrapped.
    GetImage(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedImage {
    /// Path relative to the session state dir, e.g. `images/<file>.png`.
    /// The canonical reference for re-fetching this image via
    /// `GetImage` after a session restore.
    pub image_id: String,
    pub mime: String,
    /// Base64-encoded image bytes — the iframe renders via `data:` URL.
    pub bytes_b64: String,
    /// Seed actually used (echoed so the UI can show / re-seed).
    pub seed: u64,
    /// Wall-clock ms for this generation (for the whole turn — not
    /// per-image; ComfyUI batches them in one graph).
    pub ms: u32,
}

/// One persisted turn — prompt + params + the image refs it produced.
/// Returned by `LoadTranscript` so the UI can rebuild the scrollback
/// without re-encoding image bytes for every historical turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptEntry {
    pub prompt: String,
    pub negative_prompt: String,
    pub width: u32,
    pub height: u32,
    pub steps: u32,
    pub cfg: f32,
    /// RFC3339 timestamp of when this turn was started.
    pub started_at: String,
    pub status: TranscriptStatus,
    /// Graph builder used for this turn. Defaults to schnell when
    /// loading older on-disk transcripts that predate this field —
    /// every pre-existing turn was schnell since that's all the
    /// workflow shipped at the time.
    #[serde(default = "default_model_id_back_compat")]
    pub model_id: String,
}

fn default_model_id_back_compat() -> String {
    MODEL_FLUX_SCHNELL.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TranscriptStatus {
    Done { images: Vec<TranscriptImage> },
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptImage {
    /// Path relative to the session state dir.
    pub image_id: String,
    pub mime: String,
    pub seed: u64,
    pub ms: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ImageOk {
    /// One or more generated images. Length always >= 1; the UI grids
    /// when count > 1.
    Images(Vec<GeneratedImage>),
    Settings(ImageSettings),
    SettingsUpdated,
    Health { reachable: bool, message: String },
    Transcript(Vec<TranscriptEntry>),
    /// Single image bytes for `GetImage`. Same shape as the per-image
    /// payload in `Generate`'s response, modulo `ms` (which becomes
    /// 0 on a restore — we don't track historical generation latency
    /// across reloads).
    Image(GeneratedImage),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ImageError {
    /// Could not even reach the ComfyUI HTTP endpoint.
    ComfyUnreachable(String),
    /// ComfyUI accepted the prompt but execution failed (missing
    /// checkpoint, OOM, sampler error, …). Message is whatever the
    /// `/history` status reports.
    Comfy(String),
    Internal(String),
}

pub type ImageResponse = Result<ImageOk, ImageError>;

/// Broadcast events streamed from engine → workflow alongside the
/// request/response channel. `job_id` is the ComfyUI `prompt_id` echoed
/// verbatim, so the UI can correlate events with the just-submitted
/// turn (sent via `JobQueued` immediately after a successful POST).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ImageEvent {
    JobQueued {
        job_id: String,
    },
    JobProgress {
        job_id: String,
        step: u32,
        total: u32,
    },
    JobDone {
        job_id: String,
    },
    JobError {
        job_id: String,
        message: String,
    },
}

impl Default for ImageSettings {
    fn default() -> Self {
        Self {
            comfyui_url: "http://127.0.0.1:8188".to_string(),
            default_width: 1024,
            default_height: 1024,
            default_count: 1,
            default_steps: 4,
            default_cfg: 1.0,
            default_model_id: MODEL_FLUX_SCHNELL.to_string(),
        }
    }
}

pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, postcard::Error> {
    postcard::to_allocvec(value)
}

pub fn decode<'de, T: Deserialize<'de>>(bytes: &'de [u8]) -> Result<T, postcard::Error> {
    postcard::from_bytes(bytes)
}
