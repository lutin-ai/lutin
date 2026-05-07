//! Local whisper.cpp transcription.
//!
//! `WhisperModel` is the closed set of models the desktop knows how to
//! download — encoding the catalogue as an enum means a path-traversal
//! attempt from JS (`"../../etc/passwd"`) can't even deserialize.
//! `WhisperTranscriber` keeps a `Mutex<State>` over a small `Unloaded |
//! Loaded { model, ctx }` so the model on disk and the context in
//! memory can never disagree, and the lock is never held across an
//! `.await` — downloads + context construction happen lock-free, then a
//! short re-lock installs the result.

use std::io::Write;
use std::num::NonZeroU8;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use futures_util::StreamExt;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tokio::fs;
use tracing::{info, warn};
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};

/// Closed catalogue of whisper.cpp models the desktop can download.
/// Adding a new entry is the only way to make a new filename
/// trustworthy — the JS surface deserializes into this enum, so a
/// random `String` from the frontend has no way to reach disk paths or
/// URLs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WhisperModel {
    LargeV3Turbo,
    DistilLargeV3,
}

impl Default for WhisperModel {
    fn default() -> Self {
        Self::LargeV3Turbo
    }
}

impl WhisperModel {
    pub const ALL: &'static [Self] = &[Self::LargeV3Turbo, Self::DistilLargeV3];

    pub fn filename(self) -> &'static str {
        match self {
            Self::LargeV3Turbo => "ggml-large-v3-turbo.bin",
            Self::DistilLargeV3 => "ggml-distil-large-v3.bin",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::LargeV3Turbo => "Large V3 Turbo",
            Self::DistilLargeV3 => "Distil Large V3 (English-optimised)",
        }
    }

    pub fn url(self) -> &'static str {
        match self {
            Self::LargeV3Turbo => {
                "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin"
            }
            Self::DistilLargeV3 => {
                "https://huggingface.co/distil-whisper/distil-large-v3-ggml/resolve/main/ggml-distil-large-v3.bin"
            }
        }
    }

    /// Look up a known model by its on-disk filename. Returns `None`
    /// for filenames the catalogue doesn't know about — used by
    /// `list_local_models` to ignore stray `.bin` files.
    pub fn from_filename(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|m| m.filename() == name)
    }
}

/// Sampling strategy for one transcription call. `Greedy` is faster
/// and sufficient for short clips; `Beam(n)` trades CPU for accuracy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BeamSize {
    Greedy,
    Beam(NonZeroU8),
}

impl Default for BeamSize {
    fn default() -> Self {
        // Mirrors the old desktop default; whisper.cpp's `BeamSearch`
        // with width=5 is the cost/quality knee on large-v3-turbo.
        Self::Beam(NonZeroU8::new(5).expect("5 != 0"))
    }
}

impl BeamSize {
    fn into_strategy(self) -> SamplingStrategy {
        match self {
            Self::Greedy => SamplingStrategy::Greedy { best_of: 1 },
            Self::Beam(n) => SamplingStrategy::BeamSearch {
                beam_size: n.get() as i32,
                patience: -1.0,
            },
        }
    }
}

// Persisted as a plain integer so the JSON config stays human-editable
// and forward-compatible with the old desktop's schema. `0` and `1`
// round-trip to `Greedy`; values up to 255 become `Beam(n)`.
impl Serialize for BeamSize {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Greedy => 1u8.serialize(s),
            Self::Beam(n) => n.get().serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for BeamSize {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let n = u8::deserialize(d)?;
        Ok(match n {
            0 | 1 => Self::Greedy,
            n => Self::Beam(NonZeroU8::new(n).expect("n > 1")),
        })
    }
}

/// User-tunable transcription parameters. Persisted in `DesktopSettings`
/// — the single source of truth. The transcriber doesn't keep its own
/// copy; dispatch reads this and passes it per-call.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WhisperConfig {
    pub model: WhisperModel,
    /// Whisper language code ("en", "sv", …) or `None` for autodetect.
    pub language: Option<String>,
    pub beam_size: BeamSize,
}

impl Default for WhisperConfig {
    fn default() -> Self {
        Self {
            model: WhisperModel::default(),
            language: None,
            beam_size: BeamSize::default(),
        }
    }
}

pub fn models_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".lutin")
        .join("models")
        .join("whisper")
}

fn model_path(model: WhisperModel) -> PathBuf {
    models_dir().join(model.filename())
}

/// Subset of `WhisperModel::ALL` whose `.bin` is on disk. `Vec` rather
/// than `Iterator` so it crosses the Tauri command boundary cleanly.
pub fn list_local_models() -> Vec<WhisperModel> {
    WhisperModel::ALL
        .iter()
        .copied()
        .filter(|m| model_path(*m).exists())
        .collect()
}

/// Download the model if missing, atomic-rename into place, return the
/// final path. Existing files are reused unconditionally — re-validation
/// is too slow for the hot path; corrupt files surface as a context
/// load failure later.
pub async fn ensure_model(model: WhisperModel) -> Result<PathBuf> {
    let path = model_path(model);
    if path.exists() {
        return Ok(path);
    }
    let dir = models_dir();
    fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("create {}", dir.display()))?;
    info!(model = ?model, url = model.url(), "downloading whisper model");
    download_streaming(model.url(), &path).await?;
    info!(model = ?model, path = %path.display(), "whisper model ready");
    Ok(path)
}

/// Stream the response body to disk in chunks so peak memory stays at
/// one chunk rather than the full ~1.6 GB model. Writes go to a sibling
/// `.tmp` file and `rename` lands the final atomic.
async fn download_streaming(url: &str, dest: &Path) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60 * 30))
        .build()?;
    let response = client.get(url).send().await?.error_for_status()?;
    let total = response.content_length();
    let tmp = PathBuf::from(format!("{}.tmp", dest.display()));
    let tmp_for_open = tmp.clone();
    let (writer_tx, mut writer_rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(8);
    let writer_task = tokio::task::spawn_blocking(move || -> std::io::Result<u64> {
        let mut file = std::fs::File::create(&tmp_for_open)?;
        let mut written = 0u64;
        while let Some(chunk) = writer_rx.blocking_recv() {
            file.write_all(&chunk)?;
            written += chunk.len() as u64;
        }
        file.sync_all()?;
        Ok(written)
    });

    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if writer_tx.send(chunk).await.is_err() {
            // Writer task died; surface the error from its join.
            break;
        }
    }
    drop(writer_tx);
    let written = writer_task
        .await
        .map_err(|e| anyhow!("download writer panicked: {e}"))??;

    if let Some(expected) = total
        && written != expected
    {
        // Leave the .tmp on disk for inspection rather than
        // half-renaming a truncated file into place.
        return Err(anyhow!(
            "size mismatch: expected {expected}, wrote {written}"
        ));
    }
    fs::rename(&tmp, dest).await?;
    Ok(())
}

/// One-shot whisper.cpp log silencer. Call exactly once at startup —
/// `whisper-rs::set_log_callback` is process-global.
pub fn install_log_callback() {
    unsafe {
        whisper_rs::set_log_callback(Some(whisper_log_noop), std::ptr::null_mut());
    }
}

unsafe extern "C" fn whisper_log_noop(
    _level: std::ffi::c_uint,
    _msg: *const std::ffi::c_char,
    _user_data: *mut std::ffi::c_void,
) {
}

/// In-memory model state. The two arms make divergence between "what's
/// loaded" and "which model" impossible — there's no spot to wedge a
/// `Loaded { ctx }` whose filename disagrees with `model`.
enum State {
    Unloaded,
    Loaded {
        model: WhisperModel,
        ctx: Arc<WhisperContext>,
    },
}

/// Real transcriber backed by whisper.cpp via `whisper-rs`. The state
/// `Mutex` is std (not async) because it never spans an `.await`:
/// downloads and context construction happen lock-free, the lock is
/// only taken around quick reads/writes of the `State` slot.
pub struct WhisperTranscriber {
    state: Mutex<State>,
}

impl WhisperTranscriber {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State::Unloaded),
        }
    }

    fn current(&self) -> Option<(WhisperModel, Arc<WhisperContext>)> {
        let guard = self.state.lock().expect("whisper state poisoned");
        match &*guard {
            State::Unloaded => None,
            State::Loaded { model, ctx } => Some((*model, ctx.clone())),
        }
    }

    fn install(&self, model: WhisperModel, ctx: Arc<WhisperContext>) -> Arc<WhisperContext> {
        let mut guard = self.state.lock().expect("whisper state poisoned");
        // Last-writer wins. If two concurrent loads landed for the
        // same model, both `Arc<WhisperContext>` are equivalent —
        // returning the one we just installed avoids dropping it on
        // the floor. Different models: the later call wins, which
        // matches "config changed during in-flight load".
        *guard = State::Loaded {
            model,
            ctx: ctx.clone(),
        };
        ctx
    }

    /// Resolve `model` to a usable context. Returns the cached context
    /// when it matches; otherwise downloads + loads (lock-free) then
    /// installs.
    pub async fn ensure_ctx(&self, model: WhisperModel) -> Result<Arc<WhisperContext>> {
        if let Some((m, ctx)) = self.current()
            && m == model
        {
            return Ok(ctx);
        }
        let path = ensure_model(model).await?;
        let ctx = tokio::task::spawn_blocking(move || -> Result<WhisperContext> {
            WhisperContext::new_with_params(
                path.to_str().unwrap_or_default(),
                WhisperContextParameters::default(),
            )
            .with_context(|| format!("load whisper model {}", path.display()))
        })
        .await
        .map_err(|e| anyhow!("whisper load task panicked: {e}"))??;
        Ok(self.install(model, Arc::new(ctx)))
    }

    /// Pre-load a model so the first transcribe doesn't pay the cost.
    /// Failures are returned as values; the call site decides whether
    /// to log-and-continue or surface to the user.
    pub async fn warmup(&self, model: WhisperModel) -> Result<()> {
        self.ensure_ctx(model).await.map(|_| ())
    }

    /// Run inference. `pcm` is 16 kHz mono f32 in `[-1.0, 1.0]`. Tiny
    /// clips (< ~250 ms) short-circuit to empty so a stray hotkey tap
    /// doesn't surface a hallucinated word.
    pub async fn transcribe(&self, pcm: &[f32], cfg: &WhisperConfig) -> Result<String> {
        const MIN_SAMPLES: usize = 4_000;
        if pcm.len() < MIN_SAMPLES {
            return Ok(String::new());
        }
        let ctx = self.ensure_ctx(cfg.model).await?;
        let language = cfg.language.clone();
        let beam_size = cfg.beam_size;
        let audio = pcm.to_vec();
        let text = tokio::task::spawn_blocking(move || -> Result<String> {
            let mut state: WhisperState = ctx.create_state()?;
            let mut params = FullParams::new(beam_size.into_strategy());
            params.set_single_segment(true);
            params.set_no_timestamps(true);
            params.set_temperature(0.0);
            params.set_no_context(true);
            params.set_suppress_nst(true);
            params.set_print_progress(false);
            params.set_print_special(false);
            params.set_print_realtime(false);
            if let Some(ref lang) = language {
                params.set_language(Some(lang.as_str()));
            }
            state.full(params, &audio)?;
            let mut out = String::with_capacity(128);
            for seg in state.as_iter() {
                out.push_str(&seg.to_str_lossy()?);
            }
            Ok(out.trim().to_owned())
        })
        .await
        .map_err(|e| anyhow!("whisper inference task panicked: {e}"))??;
        if text.is_empty() {
            warn!(samples = pcm.len(), "whisper returned empty transcription");
        }
        Ok(text)
    }
}
